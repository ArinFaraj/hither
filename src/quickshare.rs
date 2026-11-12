use crate::localsend::{Mode, ask};
use crate::store;
use anyhow::{Context, Result, anyhow, bail, ensure};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use data_encoding::BASE64URL_NOPAD;
use prost::Message;
use ring::{agreement, hmac, rand::SystemRandom};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

const SERVICE: &str = "_FC9F5ED42C8A._tcp.local.";
const MAX_FRAME: usize = 5 * 1024 * 1024;
const P256_SHA512: i32 = 100;
const NEXT_PROTOCOL: &str = "AES_256_CBC-HMAC_SHA256";
const D2D_SALT: [u8; 32] = [
    0x82, 0xAA, 0x55, 0xA0, 0xD3, 0x97, 0xF8, 0x83, 0x46, 0xCA, 0x1C, 0xEE, 0x8D, 0x39, 0x09, 0xB9, 0x5F, 0x13, 0xFA, 0x7D, 0xEB,
    0x1D, 0x4A, 0xB3, 0x83, 0x76, 0xB8, 0x25, 0x6D, 0xA8, 0x55, 0x10,
];

pub trait Host: Send + Sync + 'static {
    fn alias(&self) -> String;
    fn inbox(&self) -> PathBuf;
    fn mode(&self) -> Mode;
    fn received(&self, from: &str, path: &Path);
    fn text(&self, from: &str, text: &str);
}

mod pb {
    use prost::Message;

    #[derive(Clone, PartialEq, Message)]
    pub struct OfflineFrame {
        #[prost(int32, optional, tag = "1")]
        pub version: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub v1: Option<V1Frame>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct V1Frame {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub connection_request: Option<ConnectionRequestFrame>,
        #[prost(message, optional, tag = "3")]
        pub connection_response: Option<ConnectionResponseFrame>,
        #[prost(message, optional, tag = "4")]
        pub payload_transfer: Option<PayloadTransferFrame>,
        #[prost(message, optional, tag = "6")]
        pub keep_alive: Option<KeepAliveFrame>,
        #[prost(message, optional, tag = "7")]
        pub disconnection: Option<DisconnectionFrame>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ConnectionRequestFrame {
        #[prost(string, optional, tag = "1")]
        pub endpoint_id: Option<String>,
        #[prost(string, optional, tag = "2")]
        pub endpoint_name: Option<String>,
        #[prost(int32, optional, tag = "4")]
        pub nonce: Option<i32>,
        #[prost(int32, repeated, packed = "false", tag = "5")]
        pub mediums: Vec<i32>,
        #[prost(bytes = "vec", optional, tag = "6")]
        pub endpoint_info: Option<Vec<u8>>,
        #[prost(int32, optional, tag = "8")]
        pub keep_alive_interval_millis: Option<i32>,
        #[prost(int32, optional, tag = "9")]
        pub keep_alive_timeout_millis: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct ConnectionResponseFrame {
        #[prost(int32, optional, tag = "1")]
        pub status: Option<i32>,
        #[prost(int32, optional, tag = "3")]
        pub response: Option<i32>,
        #[prost(message, optional, tag = "4")]
        pub os_info: Option<OsInfo>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct OsInfo {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PayloadTransferFrame {
        #[prost(int32, optional, tag = "1")]
        pub packet_type: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub payload_header: Option<PayloadHeader>,
        #[prost(message, optional, tag = "3")]
        pub payload_chunk: Option<PayloadChunk>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PayloadHeader {
        #[prost(int64, optional, tag = "1")]
        pub id: Option<i64>,
        #[prost(int32, optional, tag = "2")]
        pub r#type: Option<i32>,
        #[prost(int64, optional, tag = "3")]
        pub total_size: Option<i64>,
        #[prost(bool, optional, tag = "4")]
        pub is_sensitive: Option<bool>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PayloadChunk {
        #[prost(int32, optional, tag = "1")]
        pub flags: Option<i32>,
        #[prost(int64, optional, tag = "2")]
        pub offset: Option<i64>,
        #[prost(bytes = "vec", optional, tag = "3")]
        pub body: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct KeepAliveFrame {
        #[prost(bool, optional, tag = "1")]
        pub ack: Option<bool>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct DisconnectionFrame {}

    #[derive(Clone, PartialEq, Message)]
    pub struct Ukey2Message {
        #[prost(int32, optional, tag = "1")]
        pub message_type: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub message_data: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Ukey2Alert {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Ukey2ClientInit {
        #[prost(int32, optional, tag = "1")]
        pub version: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub random: Option<Vec<u8>>,
        #[prost(message, repeated, tag = "3")]
        pub cipher_commitments: Vec<CipherCommitment>,
        #[prost(string, optional, tag = "4")]
        pub next_protocol: Option<String>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct CipherCommitment {
        #[prost(int32, optional, tag = "1")]
        pub handshake_cipher: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub commitment: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Ukey2ServerInit {
        #[prost(int32, optional, tag = "1")]
        pub version: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub random: Option<Vec<u8>>,
        #[prost(int32, optional, tag = "3")]
        pub handshake_cipher: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "4")]
        pub public_key: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Ukey2ClientFinished {
        #[prost(bytes = "vec", optional, tag = "1")]
        pub public_key: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct GenericPublicKey {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub ec_p256_public_key: Option<EcP256PublicKey>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct EcP256PublicKey {
        #[prost(bytes = "vec", optional, tag = "1")]
        pub x: Option<Vec<u8>>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub y: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct SecureMessage {
        #[prost(bytes = "vec", optional, tag = "1")]
        pub header_and_body: Option<Vec<u8>>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub signature: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct HeaderAndBody {
        #[prost(message, optional, tag = "1")]
        pub header: Option<Header>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub body: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Header {
        #[prost(int32, optional, tag = "1")]
        pub signature_scheme: Option<i32>,
        #[prost(int32, optional, tag = "2")]
        pub encryption_scheme: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "5")]
        pub iv: Option<Vec<u8>>,
        #[prost(bytes = "vec", optional, tag = "6")]
        pub public_metadata: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct GcmMetadata {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
        #[prost(int32, optional, tag = "2")]
        pub version: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct DeviceToDeviceMessage {
        #[prost(bytes = "vec", optional, tag = "1")]
        pub message: Option<Vec<u8>>,
        #[prost(int32, optional, tag = "2")]
        pub sequence_number: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct SharingFrame {
        #[prost(int32, optional, tag = "1")]
        pub version: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub v1: Option<SharingV1Frame>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct SharingV1Frame {
        #[prost(int32, optional, tag = "1")]
        pub r#type: Option<i32>,
        #[prost(message, optional, tag = "2")]
        pub introduction: Option<IntroductionFrame>,
        #[prost(message, optional, tag = "3")]
        pub connection_response: Option<SharingResponseFrame>,
        #[prost(message, optional, tag = "4")]
        pub paired_key_encryption: Option<PairedKeyEncryptionFrame>,
        #[prost(message, optional, tag = "5")]
        pub paired_key_result: Option<PairedKeyResultFrame>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct IntroductionFrame {
        #[prost(message, repeated, tag = "1")]
        pub file_metadata: Vec<FileMetadata>,
        #[prost(message, repeated, tag = "2")]
        pub text_metadata: Vec<TextMetadata>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct FileMetadata {
        #[prost(string, optional, tag = "1")]
        pub name: Option<String>,
        #[prost(int32, optional, tag = "2")]
        pub r#type: Option<i32>,
        #[prost(int64, optional, tag = "3")]
        pub payload_id: Option<i64>,
        #[prost(int64, optional, tag = "4")]
        pub size: Option<i64>,
        #[prost(string, optional, tag = "5")]
        pub mime_type: Option<String>,
        #[prost(int64, optional, tag = "6")]
        pub id: Option<i64>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct TextMetadata {
        #[prost(string, optional, tag = "2")]
        pub text_title: Option<String>,
        #[prost(int32, optional, tag = "3")]
        pub r#type: Option<i32>,
        #[prost(int64, optional, tag = "4")]
        pub payload_id: Option<i64>,
        #[prost(int64, optional, tag = "5")]
        pub size: Option<i64>,
        #[prost(int64, optional, tag = "6")]
        pub id: Option<i64>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct SharingResponseFrame {
        #[prost(int32, optional, tag = "1")]
        pub status: Option<i32>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PairedKeyEncryptionFrame {
        #[prost(bytes = "vec", optional, tag = "1")]
        pub signed_data: Option<Vec<u8>>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub secret_id_hash: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PairedKeyResultFrame {
        #[prost(int32, optional, tag = "1")]
        pub status: Option<i32>,
    }
}

mod kind {
    pub const CONNECTION_REQUEST: i32 = 1;
    pub const CONNECTION_RESPONSE: i32 = 2;
    pub const PAYLOAD_TRANSFER: i32 = 3;
    pub const KEEP_ALIVE: i32 = 5;
    pub const DISCONNECTION: i32 = 6;

    pub const CLIENT_INIT: i32 = 2;
    pub const SERVER_INIT: i32 = 3;
    pub const CLIENT_FINISH: i32 = 4;

    pub const BYTES: i32 = 1;
    pub const FILE: i32 = 2;
    pub const LAST_CHUNK: i32 = 1;

    pub const INTRODUCTION: i32 = 1;
    pub const RESPONSE: i32 = 2;
    pub const PAIRED_KEY_ENCRYPTION: i32 = 3;
    pub const PAIRED_KEY_RESULT: i32 = 4;
    pub const CANCEL: i32 = 6;

    pub const ACCEPT: i32 = 1;
    pub const REJECT: i32 = 2;
    pub const UNSUPPORTED: i32 = 4;
    pub const UNABLE: i32 = 3;
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data);
    tag.as_ref().try_into().expect("sha256 length")
}

fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let prk = hmac_sha256(salt, ikm);
    let mut block = info.to_vec();
    block.push(1);
    hmac_sha256(&prk, &block)
}

fn pin(auth: &[u8]) -> String {
    let (mut hash, mut mul) = (0i64, 1i64);
    for &b in auth {
        hash = (hash + b as i8 as i64 * mul) % 9973;
        mul = (mul * 31) % 9973;
    }
    format!("{:04}", hash.abs())
}

fn signed_be(b: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(b.len() + 1);
    if b[0] & 0x80 != 0 {
        v.push(0);
    }
    v.extend_from_slice(b);
    v
}

fn coord(b: &[u8]) -> [u8; 32] {
    let b = if b.len() > 32 { &b[b.len() - 32..] } else { b };
    let mut out = [0u8; 32];
    out[32 - b.len()..].copy_from_slice(b);
    out
}

struct Keys {
    enc: [u8; 32],
    dec: [u8; 32],
    send_mac: [u8; 32],
    recv_mac: [u8; 32],
    pin: String,
}

fn derive(shared: &[u8], client_init: &[u8], server_init: &[u8], server: bool) -> Keys {
    let secret = Sha256::digest(shared);
    let info = [client_init, server_init].concat();
    let auth = hkdf(&secret, b"UKEY2 v1 auth", &info);
    let next = hkdf(&secret, b"UKEY2 v1 next", &info);
    let client = hkdf(&next, &D2D_SALT, b"client");
    let srv = hkdf(&next, &D2D_SALT, b"server");
    let salt = Sha256::digest(b"SecureMessage");
    let (c_enc, c_mac) = (hkdf(&client, &salt, b"ENC:2"), hkdf(&client, &salt, b"SIG:1"));
    let (s_enc, s_mac) = (hkdf(&srv, &salt, b"ENC:2"), hkdf(&srv, &salt, b"SIG:1"));
    let pin = pin(&auth);
    if server {
        Keys { enc: s_enc, dec: c_enc, send_mac: s_mac, recv_mac: c_mac, pin }
    } else {
        Keys { enc: c_enc, dec: s_enc, send_mac: c_mac, recv_mac: s_mac, pin }
    }
}

async fn read_frame(r: &mut OwnedReadHalf) -> Result<Vec<u8>> {
    let len = r.read_u32().await? as usize;
    ensure!(len <= MAX_FRAME, "frame of {len} bytes");
    let mut buf = vec![0; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_frame(w: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await?;
    Ok(())
}

fn offline(kind: i32, f: impl FnOnce(&mut pb::V1Frame)) -> pb::OfflineFrame {
    let mut v1 = pb::V1Frame { r#type: Some(kind), ..Default::default() };
    f(&mut v1);
    pb::OfflineFrame { version: Some(1), v1: Some(v1) }
}

fn sharing(kind: i32, f: impl FnOnce(&mut pb::SharingV1Frame)) -> pb::SharingFrame {
    let mut v1 = pb::SharingV1Frame { r#type: Some(kind), ..Default::default() };
    f(&mut v1);
    pb::SharingFrame { version: Some(1), v1: Some(v1) }
}

struct Channel {
    w: OwnedWriteHalf,
    keys: Keys,
    sent: i32,
    got: i32,
}

impl Channel {
    async fn send(&mut self, frame: &pb::OfflineFrame) -> Result<()> {
        self.sent += 1;
        let d2d = pb::DeviceToDeviceMessage { message: Some(frame.encode_to_vec()), sequence_number: Some(self.sent) };
        let iv: [u8; 16] = rand::random();
        let body = cbc::Encryptor::<aes::Aes256>::new(&self.keys.enc.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&d2d.encode_to_vec());
        let meta = pb::GcmMetadata { r#type: Some(13), version: Some(1) };
        let hb = pb::HeaderAndBody {
            header: Some(pb::Header {
                signature_scheme: Some(1),
                encryption_scheme: Some(2),
                iv: Some(iv.to_vec()),
                public_metadata: Some(meta.encode_to_vec()),
            }),
            body: Some(body),
        }
        .encode_to_vec();
        let signature = hmac_sha256(&self.keys.send_mac, &hb).to_vec();
        let msg = pb::SecureMessage { header_and_body: Some(hb), signature: Some(signature) };
        write_frame(&mut self.w, &msg.encode_to_vec()).await
    }

    fn open(&mut self, raw: &[u8]) -> Result<pb::OfflineFrame> {
        let msg = pb::SecureMessage::decode(raw)?;
        let hb = msg.header_and_body.context("no header_and_body")?;
        let sig = msg.signature.context("no signature")?;
        hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, &self.keys.recv_mac), &hb, &sig).map_err(|_| anyhow!("bad signature"))?;
        let hb = pb::HeaderAndBody::decode(hb.as_slice())?;
        let iv: [u8; 16] = hb.header.and_then(|h| h.iv).context("no iv")?.try_into().map_err(|_| anyhow!("bad iv"))?;
        let plain = cbc::Decryptor::<aes::Aes256>::new(&self.keys.dec.into(), &iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(&hb.body.unwrap_or_default())
            .map_err(|_| anyhow!("bad padding"))?;
        let d2d = pb::DeviceToDeviceMessage::decode(plain.as_slice())?;
        self.got += 1;
        ensure!(d2d.sequence_number == Some(self.got), "sequence {:?}, wanted {}", d2d.sequence_number, self.got);
        Ok(pb::OfflineFrame::decode(d2d.message.unwrap_or_default().as_slice())?)
    }

    async fn send_sharing(&mut self, frame: &pb::SharingFrame) -> Result<()> {
        let data = frame.encode_to_vec();
        let id: i64 = rand::random();
        let header = pb::PayloadHeader { id: Some(id), r#type: Some(kind::BYTES), total_size: Some(data.len() as i64), is_sensitive: Some(false) };
        let len = data.len() as i64;
        for chunk in [
            pb::PayloadChunk { flags: Some(0), offset: Some(0), body: Some(data) },
            pb::PayloadChunk { flags: Some(kind::LAST_CHUNK), offset: Some(len), body: None },
        ] {
            let frame = offline(kind::PAYLOAD_TRANSFER, |v| {
                v.payload_transfer = Some(pb::PayloadTransferFrame {
                    packet_type: Some(1),
                    payload_header: Some(header.clone()),
                    payload_chunk: Some(chunk),
                })
            });
            self.send(&frame).await?;
        }
        Ok(())
    }

    async fn keep_alive(&mut self, ack: bool) -> Result<()> {
        self.send(&offline(kind::KEEP_ALIVE, |v| v.keep_alive = Some(pb::KeepAliveFrame { ack: Some(ack) }))).await
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.send(&offline(kind::DISCONNECTION, |v| v.disconnection = Some(pb::DisconnectionFrame {}))).await?;
        let _ = self.w.shutdown().await;
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Remote {
    pub name: String,
    pub device_type: u8,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Peer {
    pub name: String,
    pub device_type: u8,
    pub addr: SocketAddr,
}

fn parse_endpoint_info(info: &[u8]) -> Result<Remote> {
    ensure!(info.len() >= 17, "endpoint info too short");
    let device_type = (info[0] >> 1) & 7;
    let name = if info[0] & 0x10 == 0 && info.len() > 17 {
        let n = info[17] as usize;
        ensure!(info.len() >= 18 + n, "endpoint name truncated");
        String::from_utf8_lossy(&info[18..18 + n]).into_owned()
    } else {
        String::new()
    };
    Ok(Remote { name: if name.is_empty() { "Android".into() } else { name }, device_type })
}

pub fn endpoint_info(name: &str, device_type: u8) -> Vec<u8> {
    let mut v = vec![device_type << 1];
    v.extend_from_slice(&rand::random::<[u8; 16]>());
    let mut name = name.as_bytes();
    while name.len() > 255 || std::str::from_utf8(name).is_err() {
        name = &name[..name.len() - 1];
    }
    v.push(name.len() as u8);
    v.extend_from_slice(name);
    v
}

fn endpoint_id() -> [u8; 4] {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    std::array::from_fn(|_| ALPHABET[rand::random_range(0..ALPHABET.len())])
}

async fn handshake(r: &mut OwnedReadHalf, mut w: OwnedWriteHalf) -> Result<(Remote, Channel)> {
    let req = pb::OfflineFrame::decode(read_frame(r).await?.as_slice())?;
    let v1 = req.v1.context("no v1")?;
    ensure!(v1.r#type == Some(kind::CONNECTION_REQUEST), "expected a connection request, got {:?}", v1.r#type);
    let remote = parse_endpoint_info(&v1.connection_request.and_then(|c| c.endpoint_info).context("no endpoint info")?)?;

    let client_init_raw = read_frame(r).await?;
    let msg = pb::Ukey2Message::decode(client_init_raw.as_slice())?;
    ensure!(msg.message_type == Some(kind::CLIENT_INIT), "expected ukey2 client init");
    let init = pb::Ukey2ClientInit::decode(msg.message_data.unwrap_or_default().as_slice())?;
    ensure!(init.version == Some(1), "ukey2 version {:?}", init.version);
    ensure!(init.random.as_ref().is_some_and(|r| r.len() == 32), "ukey2 random");
    ensure!(init.next_protocol.as_deref() == Some(NEXT_PROTOCOL), "next protocol {:?}", init.next_protocol);
    let commitment = init
        .cipher_commitments
        .iter()
        .find(|c| c.handshake_cipher == Some(P256_SHA512))
        .and_then(|c| c.commitment.clone())
        .context("no P256_SHA512 commitment")?;

    let rng = SystemRandom::new();
    let ours = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).map_err(|_| anyhow!("keygen"))?;
    let public = ours.compute_public_key().map_err(|_| anyhow!("keygen"))?;
    let public = public.as_ref();
    let key = pb::GenericPublicKey {
        r#type: Some(1),
        ec_p256_public_key: Some(pb::EcP256PublicKey { x: Some(signed_be(&public[1..33])), y: Some(signed_be(&public[33..65])) }),
    };
    let server_init = pb::Ukey2ServerInit {
        version: Some(1),
        random: Some(rand::random::<[u8; 32]>().to_vec()),
        handshake_cipher: Some(P256_SHA512),
        public_key: Some(key.encode_to_vec()),
    };
    let server_init_raw = pb::Ukey2Message { message_type: Some(kind::SERVER_INIT), message_data: Some(server_init.encode_to_vec()) }.encode_to_vec();
    write_frame(&mut w, &server_init_raw).await?;

    let finish_raw = read_frame(r).await?;
    ensure!(Sha512::digest(&finish_raw).as_slice() == commitment.as_slice(), "ukey2 commitment mismatch");
    let msg = pb::Ukey2Message::decode(finish_raw.as_slice())?;
    ensure!(msg.message_type == Some(kind::CLIENT_FINISH), "expected ukey2 client finish");
    let finish = pb::Ukey2ClientFinished::decode(msg.message_data.unwrap_or_default().as_slice())?;
    let theirs = pb::GenericPublicKey::decode(finish.public_key.unwrap_or_default().as_slice())?;
    let p = theirs.ec_p256_public_key.context("peer key is not P-256")?;
    let mut point = vec![4u8];
    point.extend_from_slice(&coord(&p.x.unwrap_or_default()));
    point.extend_from_slice(&coord(&p.y.unwrap_or_default()));
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, point);
    let keys = agreement::agree_ephemeral(ours, &peer, |shared| derive(shared, &client_init_raw, &server_init_raw, true))
        .map_err(|_| anyhow!("bad peer key"))?;

    let resp = pb::OfflineFrame::decode(read_frame(r).await?.as_slice())?;
    ensure!(resp.v1.and_then(|v| v.r#type) == Some(kind::CONNECTION_RESPONSE), "expected a connection response");
    let ours = offline(kind::CONNECTION_RESPONSE, |v| {
        v.connection_response = Some(pb::ConnectionResponseFrame { status: Some(0), response: Some(1), os_info: Some(pb::OsInfo { r#type: Some(4) }) })
    });
    write_frame(&mut w, &ours.encode_to_vec()).await?;
    Ok((remote, Channel { w, keys, sent: 0, got: 0 }))
}

enum Incoming {
    Files(HashMap<i64, Pending>),
    Text { id: i64 },
}

struct Pending {
    name: String,
    size: i64,
    dest: PathBuf,
    part: PathBuf,
    file: Option<tokio::fs::File>,
    got: i64,
}

fn describe(intro: &pb::IntroductionFrame) -> String {
    if let Some(t) = intro.text_metadata.first() {
        return t.text_title.clone().unwrap_or_else(|| "some text".into());
    }
    let total: i64 = intro.file_metadata.iter().filter_map(|f| f.size).sum();
    match intro.file_metadata.as_slice() {
        [f] => format!("{} ({})", f.name.as_deref().unwrap_or("a file"), crate::localsend::human(total as u64)),
        fs => format!("{} files ({})", fs.len(), crate::localsend::human(total as u64)),
    }
}

pub struct QuickShare {
    pub port: u16,
    pub name: String,
    alias: String,
    mdns: mdns_sd::ServiceDaemon,
    _advert: Advert,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for QuickShare {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = self.mdns.shutdown();
    }
}

impl QuickShare {
    pub async fn start(host: Arc<dyn Host>) -> Result<Arc<Self>> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        let port = listener.local_addr()?.port();
        let id = endpoint_id();
        let mut name = vec![0x23];
        name.extend_from_slice(&id);
        name.extend_from_slice(&[0xFC, 0x9F, 0x5E, 0, 0]);
        let instance = BASE64URL_NOPAD.encode(&name);
        let device_type = if cfg!(any(target_os = "android", target_os = "ios")) { 1 } else { 3 };
        let info = BASE64URL_NOPAD.encode(&endpoint_info(&host.alias(), device_type));
        let mdns = mdns_sd::ServiceDaemon::new()?;
        let advert = Advert::new(&mdns, &instance, &id, port, &info)?;
        let peers: Arc<Mutex<HashMap<String, Peer>>> = Default::default();
        let events = mdns.browse(SERVICE)?;
        let browse = {
            let (peers, me) = (peers.clone(), format!("{instance}.{SERVICE}"));
            tokio::spawn(async move {
                while let Ok(ev) = events.recv_async().await {
                    match ev {
                        mdns_sd::ServiceEvent::ServiceResolved(s) if s.fullname != me => {
                            let Some(info) = s.get_property_val_str("n").and_then(|n| BASE64URL_NOPAD.decode(n.trim_end_matches('=').as_bytes()).ok()) else { continue };
                            let Ok(remote) = parse_endpoint_info(&info) else { continue };
                            if info[0] & 0x10 != 0 {
                                continue;
                            }
                            let Some(ip) = s.get_addresses_v4().into_iter().next() else { continue };
                            let peer = Peer { name: remote.name, device_type: remote.device_type, addr: (ip, s.port).into() };
                            peers.lock().unwrap().insert(s.fullname.clone(), peer);
                        }
                        mdns_sd::ServiceEvent::ServiceRemoved(_, full) => {
                            peers.lock().unwrap().remove(&full);
                        }
                        _ => {}
                    }
                }
            })
        };
        let alias = host.alias();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((tcp, addr)) = listener.accept().await else { continue };
                let host = host.clone();
                tokio::spawn(async move {
                    if host.mode() == Mode::Off {
                        return;
                    }
                    if let Err(e) = serve(host, tcp).await {
                        eprintln!("quickshare from {addr}: {e:#}");
                    }
                });
            }
        });
        Ok(Arc::new(QuickShare { port, name: instance, alias, mdns, _advert: advert, peers, tasks: vec![accept, browse] }))
    }

    pub fn peers(&self) -> Vec<Peer> {
        let mut v: Vec<Peer> = self.peers.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn find(&self, q: &str) -> Option<Peer> {
        let q = q.to_lowercase();
        let peers = self.peers();
        peers.iter().find(|p| p.name.to_lowercase() == q).or_else(|| peers.iter().find(|p| p.name.to_lowercase().starts_with(&q))).cloned()
    }

    pub async fn send_files(&self, peer: &Peer, paths: &[PathBuf]) -> Result<()> {
        send(peer.addr, &self.alias, Outgoing::Files(paths.to_vec())).await
    }

    pub async fn send_text(&self, peer: &Peer, text: &str) -> Result<()> {
        send(peer.addr, &self.alias, Outgoing::Text(text.to_string())).await
    }
}

#[cfg(target_os = "macos")]
struct Advert(*mut std::ffi::c_void);

#[cfg(target_os = "macos")]
unsafe impl Send for Advert {}
#[cfg(target_os = "macos")]
unsafe impl Sync for Advert {}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn DNSServiceRegister(
        sd_ref: *mut *mut std::ffi::c_void,
        flags: u32,
        interface_index: u32,
        name: *const std::ffi::c_char,
        regtype: *const std::ffi::c_char,
        domain: *const std::ffi::c_char,
        host: *const std::ffi::c_char,
        port: u16,
        txt_len: u16,
        txt_record: *const std::ffi::c_void,
        callback: *const std::ffi::c_void,
        context: *mut std::ffi::c_void,
    ) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: *mut std::ffi::c_void);
}

#[cfg(target_os = "macos")]
impl Advert {
    fn new(_: &mdns_sd::ServiceDaemon, instance: &str, _: &[u8; 4], port: u16, info: &str) -> Result<Self> {
        let entry = format!("n={info}");
        ensure!(entry.len() < 256, "endpoint info too long");
        let mut txt = vec![entry.len() as u8];
        txt.extend_from_slice(entry.as_bytes());
        let name = std::ffi::CString::new(instance)?;
        let ty = std::ffi::CString::new("_FC9F5ED42C8A._tcp")?;
        let mut sd = std::ptr::null_mut();
        let err = unsafe {
            DNSServiceRegister(
                &mut sd,
                0,
                0,
                name.as_ptr(),
                ty.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt.len() as u16,
                txt.as_ptr().cast(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        ensure!(err == 0 && !sd.is_null(), "DNSServiceRegister failed ({err})");
        Ok(Advert(sd))
    }
}

#[cfg(target_os = "macos")]
impl Drop for Advert {
    fn drop(&mut self) {
        unsafe { DNSServiceRefDeallocate(self.0) }
    }
}

#[cfg(not(target_os = "macos"))]
struct Advert;

#[cfg(not(target_os = "macos"))]
impl Advert {
    fn new(mdns: &mdns_sd::ServiceDaemon, instance: &str, id: &[u8; 4], port: u16, info: &str) -> Result<Self> {
        let hostname = format!("drop-{}.local.", String::from_utf8_lossy(id).to_lowercase());
        let svc = mdns_sd::ServiceInfo::new(SERVICE, instance, &hostname, "", port, &[("n", info)][..])?.enable_addr_auto();
        mdns.register(svc)?;
        Ok(Advert)
    }
}

pub enum Outgoing {
    Files(Vec<PathBuf>),
    Text(String),
}

async fn client_handshake(r: &mut OwnedReadHalf, mut w: OwnedWriteHalf, alias: &str) -> Result<Channel> {
    let id = endpoint_id();
    let req = offline(kind::CONNECTION_REQUEST, |v| {
        v.connection_request = Some(pb::ConnectionRequestFrame {
            endpoint_id: Some(String::from_utf8_lossy(&id).into_owned()),
            endpoint_name: Some(alias.to_string()),
            nonce: Some(rand::random()),
            mediums: vec![5],
            endpoint_info: Some(endpoint_info(alias, 3)),
            keep_alive_interval_millis: Some(10_000),
            keep_alive_timeout_millis: Some(30_000),
        })
    });
    write_frame(&mut w, &req.encode_to_vec()).await?;

    let rng = SystemRandom::new();
    let ours = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).map_err(|_| anyhow!("keygen"))?;
    let public = ours.compute_public_key().map_err(|_| anyhow!("keygen"))?;
    let public = public.as_ref();
    let key = pb::GenericPublicKey {
        r#type: Some(1),
        ec_p256_public_key: Some(pb::EcP256PublicKey { x: Some(signed_be(&public[1..33])), y: Some(signed_be(&public[33..65])) }),
    };
    let finish = pb::Ukey2Message {
        message_type: Some(kind::CLIENT_FINISH),
        message_data: Some(pb::Ukey2ClientFinished { public_key: Some(key.encode_to_vec()) }.encode_to_vec()),
    }
    .encode_to_vec();
    let init = pb::Ukey2ClientInit {
        version: Some(1),
        random: Some(rand::random::<[u8; 32]>().to_vec()),
        cipher_commitments: vec![pb::CipherCommitment { handshake_cipher: Some(P256_SHA512), commitment: Some(Sha512::digest(&finish).to_vec()) }],
        next_protocol: Some(NEXT_PROTOCOL.into()),
    };
    let client_init_raw = pb::Ukey2Message { message_type: Some(kind::CLIENT_INIT), message_data: Some(init.encode_to_vec()) }.encode_to_vec();
    write_frame(&mut w, &client_init_raw).await?;

    let server_init_raw = read_frame(r).await?;
    let msg = pb::Ukey2Message::decode(server_init_raw.as_slice())?;
    ensure!(msg.message_type == Some(kind::SERVER_INIT), "expected ukey2 server init, got {:?}", msg.message_type);
    let si = pb::Ukey2ServerInit::decode(msg.message_data.unwrap_or_default().as_slice())?;
    ensure!(si.handshake_cipher == Some(P256_SHA512), "server picked cipher {:?}", si.handshake_cipher);
    let theirs = pb::GenericPublicKey::decode(si.public_key.unwrap_or_default().as_slice())?;
    let p = theirs.ec_p256_public_key.context("server key is not P-256")?;
    let mut point = vec![4u8];
    point.extend_from_slice(&coord(&p.x.unwrap_or_default()));
    point.extend_from_slice(&coord(&p.y.unwrap_or_default()));
    write_frame(&mut w, &finish).await?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, point);
    let keys = agreement::agree_ephemeral(ours, &peer, |shared| derive(shared, &client_init_raw, &server_init_raw, false))
        .map_err(|_| anyhow!("bad server key"))?;

    let ours = offline(kind::CONNECTION_RESPONSE, |v| {
        v.connection_response = Some(pb::ConnectionResponseFrame { status: Some(0), response: Some(1), os_info: Some(pb::OsInfo { r#type: Some(4) }) })
    });
    write_frame(&mut w, &ours.encode_to_vec()).await?;
    let resp = pb::OfflineFrame::decode(read_frame(r).await?.as_slice())?;
    let resp = resp.v1.context("no v1")?;
    ensure!(resp.r#type == Some(kind::CONNECTION_RESPONSE), "expected a connection response");
    ensure!(resp.connection_response.and_then(|c| c.response) == Some(1), "connection refused");
    Ok(Channel { w, keys, sent: 0, got: 0 })
}

async fn next_sharing(ch: &mut Channel, r: &mut OwnedReadHalf, buffers: &mut HashMap<i64, Vec<u8>>) -> Result<pb::SharingV1Frame> {
    loop {
        let frame = ch.open(&read_frame(r).await?)?;
        let Some(v1) = frame.v1 else { continue };
        match v1.r#type {
            Some(kind::KEEP_ALIVE) => {
                if !v1.keep_alive.and_then(|k| k.ack).unwrap_or(false) {
                    ch.keep_alive(true).await?;
                }
            }
            Some(kind::DISCONNECTION) => bail!("the other side hung up"),
            Some(kind::PAYLOAD_TRANSFER) => {
                let Some(pt) = v1.payload_transfer else { continue };
                let (Some(header), Some(chunk)) = (pt.payload_header, pt.payload_chunk) else { continue };
                if header.r#type != Some(kind::BYTES) {
                    continue;
                }
                let id = header.id.unwrap_or(0);
                let buf = buffers.entry(id).or_default();
                let body = chunk.body.unwrap_or_default();
                ensure!(buf.len() + body.len() <= MAX_FRAME, "bytes payload too large");
                buf.extend_from_slice(&body);
                if chunk.flags.unwrap_or(0) & kind::LAST_CHUNK == 0 {
                    continue;
                }
                let data = buffers.remove(&id).unwrap_or_default();
                if let Some(v) = pb::SharingFrame::decode(data.as_slice()).ok().and_then(|f| f.v1) {
                    return Ok(v);
                }
            }
            _ => {}
        }
    }
}

fn mime(path: &Path) -> (&'static str, i32) {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => ("image/jpeg", 1),
        "png" => ("image/png", 1),
        "gif" => ("image/gif", 1),
        "heic" => ("image/heic", 1),
        "webp" => ("image/webp", 1),
        "mp4" => ("video/mp4", 2),
        "mov" => ("video/quicktime", 2),
        "mp3" => ("audio/mpeg", 4),
        "m4a" => ("audio/mp4", 4),
        "apk" => ("application/vnd.android.package-archive", 3),
        "pdf" => ("application/pdf", 5),
        "txt" => ("text/plain", 5),
        _ => ("application/octet-stream", 0),
    }
}

const CHUNK: usize = 512 * 1024;

async fn send_payload(ch: &mut Channel, id: i64, kind_: i32, total: i64, mut src: impl tokio::io::AsyncRead + Unpin) -> Result<()> {
    let header = pb::PayloadHeader { id: Some(id), r#type: Some(kind_), total_size: Some(total), is_sensitive: Some(false) };
    let mut offset = 0i64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let mut n = 0;
        while n < CHUNK {
            let k = src.read(&mut buf[n..]).await?;
            if k == 0 {
                break;
            }
            n += k;
        }
        if n == 0 {
            break;
        }
        let chunk = pb::PayloadChunk { flags: Some(0), offset: Some(offset), body: Some(buf[..n].to_vec()) };
        let frame = offline(kind::PAYLOAD_TRANSFER, |v| {
            v.payload_transfer = Some(pb::PayloadTransferFrame { packet_type: Some(1), payload_header: Some(header.clone()), payload_chunk: Some(chunk) })
        });
        ch.send(&frame).await?;
        offset += n as i64;
    }
    ensure!(offset == total, "file changed size while sending");
    let last = pb::PayloadChunk { flags: Some(kind::LAST_CHUNK), offset: Some(offset), body: None };
    let frame = offline(kind::PAYLOAD_TRANSFER, |v| {
        v.payload_transfer = Some(pb::PayloadTransferFrame { packet_type: Some(1), payload_header: Some(header), payload_chunk: Some(last) })
    });
    ch.send(&frame).await
}

pub async fn send(addr: SocketAddr, alias: &str, what: Outgoing) -> Result<()> {
    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await.context("connect timed out")??;
    let _ = tcp.set_nodelay(true);
    let (mut r, w) = tcp.into_split();
    let mut ch = tokio::time::timeout(Duration::from_secs(15), client_handshake(&mut r, w, alias)).await.context("handshake timed out")??;
    let mut buffers = HashMap::new();

    ch.send_sharing(&sharing(kind::PAIRED_KEY_ENCRYPTION, |v| {
        v.paired_key_encryption = Some(pb::PairedKeyEncryptionFrame {
            signed_data: Some(rand::random::<[u8; 72]>().to_vec()),
            secret_id_hash: Some(rand::random::<[u8; 6]>().to_vec()),
        })
    }))
    .await?;
    while next_sharing(&mut ch, &mut r, &mut buffers).await?.r#type != Some(kind::PAIRED_KEY_ENCRYPTION) {}
    ch.send_sharing(&sharing(kind::PAIRED_KEY_RESULT, |v| v.paired_key_result = Some(pb::PairedKeyResultFrame { status: Some(kind::UNABLE) }))).await?;
    while next_sharing(&mut ch, &mut r, &mut buffers).await?.r#type != Some(kind::PAIRED_KEY_RESULT) {}

    let mut files = Vec::new();
    let intro = match &what {
        Outgoing::Text(t) => {
            let url = !t.contains(char::is_whitespace) && (t.starts_with("http://") || t.starts_with("https://"));
            pb::IntroductionFrame {
                text_metadata: vec![pb::TextMetadata {
                    text_title: Some(t.chars().take(40).collect()),
                    r#type: Some(if url { 2 } else { 1 }),
                    payload_id: Some(rand::random()),
                    size: Some(t.len() as i64),
                    id: Some(rand::random()),
                }],
                ..Default::default()
            }
        }
        Outgoing::Files(paths) => {
            let mut meta = Vec::new();
            for p in paths {
                let size = tokio::fs::metadata(p).await.with_context(|| p.display().to_string())?.len() as i64;
                let name = p.file_name().context("no file name")?.to_string_lossy().into_owned();
                let (mime, ty) = mime(p);
                let payload_id: i64 = rand::random();
                files.push((p.clone(), payload_id, size));
                meta.push(pb::FileMetadata {
                    name: Some(name),
                    r#type: Some(ty),
                    payload_id: Some(payload_id),
                    size: Some(size),
                    mime_type: Some(mime.into()),
                    id: Some(rand::random()),
                });
            }
            pb::IntroductionFrame { file_metadata: meta, ..Default::default() }
        }
    };
    let text_id = intro.text_metadata.first().and_then(|t| t.payload_id);
    ch.send_sharing(&sharing(kind::INTRODUCTION, |v| v.introduction = Some(intro))).await?;

    let answer = tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            let f = next_sharing(&mut ch, &mut r, &mut buffers).await?;
            match f.r#type {
                Some(kind::RESPONSE) => return Ok::<_, anyhow::Error>(f.connection_response.and_then(|c| c.status).unwrap_or(0)),
                Some(kind::CANCEL) => bail!("the other side cancelled"),
                _ => {}
            }
        }
    })
    .await
    .context("no answer from the other side")??;
    match answer {
        kind::ACCEPT => {}
        kind::REJECT => bail!("declined"),
        3 => bail!("not enough space on the other side"),
        s => bail!("refused (status {s})"),
    }

    match what {
        Outgoing::Text(t) => {
            let bytes = t.into_bytes();
            send_payload(&mut ch, text_id.unwrap_or(0), kind::BYTES, bytes.len() as i64, bytes.as_slice()).await?;
        }
        Outgoing::Files(_) => {
            for (path, id, size) in files {
                let f = tokio::fs::File::open(&path).await?;
                send_payload(&mut ch, id, kind::FILE, size, f).await?;
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ch.open(&read_frame(&mut r).await?)?.v1.and_then(|v| v.r#type) {
                Some(kind::DISCONNECTION) => return Ok::<_, anyhow::Error>(()),
                Some(kind::KEEP_ALIVE) => ch.keep_alive(true).await?,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.disconnect().await;
    Ok(())
}

async fn serve(host: Arc<dyn Host>, tcp: TcpStream) -> Result<()> {
    let _ = tcp.set_nodelay(true);
    let (mut r, w) = tcp.into_split();
    let (remote, mut ch) = tokio::time::timeout(Duration::from_secs(15), handshake(&mut r, w)).await.context("handshake timed out")??;

    let (frames_tx, mut frames) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(16);
    tokio::spawn(async move {
        loop {
            let f = read_frame(&mut r).await;
            let stop = f.is_err();
            if frames_tx.send(f).await.is_err() || stop {
                break;
            }
        }
    });

    ch.send_sharing(&sharing(kind::PAIRED_KEY_ENCRYPTION, |v| {
        v.paired_key_encryption = Some(pb::PairedKeyEncryptionFrame {
            signed_data: Some(rand::random::<[u8; 72]>().to_vec()),
            secret_id_hash: Some(rand::random::<[u8; 6]>().to_vec()),
        })
    }))
    .await?;

    let mut buffers: HashMap<i64, Vec<u8>> = HashMap::new();
    let mut incoming: Option<Incoming> = None;
    let (consent_tx, mut consent) = tokio::sync::mpsc::channel::<bool>(1);
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.tick().await;
    let result = loop {
        tokio::select! {
            _ = tick.tick() => ch.keep_alive(false).await?,
            Some(ok) = consent.recv() => {
                match incoming.as_mut() {
                    Some(Incoming::Files(files)) if ok => {
                        for p in files.values_mut() {
                            p.file = Some(tokio::fs::File::create(&p.part).await.with_context(|| format!("create {}", p.part.display()))?);
                        }
                    }
                    _ => {}
                }
                let status = if ok { kind::ACCEPT } else { kind::REJECT };
                ch.send_sharing(&sharing(kind::RESPONSE, |v| v.connection_response = Some(pb::SharingResponseFrame { status: Some(status) }))).await?;
                if !ok {
                    break Ok(());
                }
            }
            f = frames.recv() => {
                let Some(raw) = f else { break Err(anyhow!("connection closed")) };
                let frame = ch.open(&raw?)?;
                let Some(v1) = frame.v1 else { continue };
                match v1.r#type {
                    Some(kind::KEEP_ALIVE) => {
                        if !v1.keep_alive.and_then(|k| k.ack).unwrap_or(false) {
                            ch.keep_alive(true).await?;
                        }
                    }
                    Some(kind::DISCONNECTION) => break if matches!(&incoming, Some(Incoming::Files(f)) if !f.is_empty()) { Err(anyhow!("sender hung up")) } else { Ok(()) },
                    Some(kind::PAYLOAD_TRANSFER) => {
                        let Some(pt) = v1.payload_transfer else { continue };
                        let header = pt.payload_header.context("no payload header")?;
                        let chunk = pt.payload_chunk.context("no payload chunk")?;
                        let id = header.id.context("no payload id")?;
                        let last = chunk.flags.unwrap_or(0) & kind::LAST_CHUNK != 0;
                        let body = chunk.body.unwrap_or_default();
                        if header.r#type == Some(kind::FILE) {
                            let Some(Incoming::Files(files)) = incoming.as_mut() else { bail!("file data before an introduction") };
                            let p = files.get_mut(&id).context("unknown file payload")?;
                            let file = p.file.as_mut().context("file data before consent")?;
                            ensure!(chunk.offset == Some(p.got), "chunk offset {:?}, wanted {}", chunk.offset, p.got);
                            ensure!(p.got + body.len() as i64 <= p.size, "{} is longer than announced", p.name);
                            file.write_all(&body).await?;
                            p.got += body.len() as i64;
                            if last {
                                let mut p = files.remove(&id).expect("present");
                                if let Some(mut f) = p.file.take() {
                                    f.flush().await?;
                                }
                                tokio::fs::rename(&p.part, &p.dest).await?;
                                host.received(&remote.name, &p.dest);
                                if files.is_empty() {
                                    ch.disconnect().await?;
                                    break Ok(());
                                }
                            }
                            continue;
                        }
                        let buf = buffers.entry(id).or_default();
                        ensure!(chunk.offset == Some(buf.len() as i64), "bytes offset");
                        ensure!(buf.len() + body.len() <= MAX_FRAME, "bytes payload too large");
                        buf.extend_from_slice(&body);
                        if !last {
                            continue;
                        }
                        let data = buffers.remove(&id).unwrap_or_default();
                        if let Some(Incoming::Text { id: tid }) = &incoming {
                            if *tid == id {
                                let text = String::from_utf8_lossy(&data).into_owned();
                                host.text(&remote.name, &text);
                                ch.disconnect().await?;
                                break Ok(());
                            }
                        }
                        let Ok(sf) = pb::SharingFrame::decode(data.as_slice()) else { continue };
                        let Some(sv) = sf.v1 else { continue };
                        match sv.r#type {
                            Some(kind::PAIRED_KEY_ENCRYPTION) => {
                                ch.send_sharing(&sharing(kind::PAIRED_KEY_RESULT, |v| v.paired_key_result = Some(pb::PairedKeyResultFrame { status: Some(kind::UNABLE) }))).await?;
                            }
                            Some(kind::PAIRED_KEY_RESULT) => {}
                            Some(kind::CANCEL) => break Err(anyhow!("sender cancelled")),
                            Some(kind::INTRODUCTION) => {
                                let intro = sv.introduction.unwrap_or_default();
                                let inbox = host.inbox();
                                let _ = tokio::fs::create_dir_all(&inbox).await;
                                if let Some(t) = intro.text_metadata.first().filter(|_| intro.file_metadata.is_empty()) {
                                    if !matches!(t.r#type, Some(1) | Some(2)) {
                                        ch.send_sharing(&sharing(kind::RESPONSE, |v| v.connection_response = Some(pb::SharingResponseFrame { status: Some(kind::UNSUPPORTED) }))).await?;
                                        ch.disconnect().await?;
                                        break Ok(());
                                    }
                                    incoming = Some(Incoming::Text { id: t.payload_id.context("no text payload id")? });
                                } else if !intro.file_metadata.is_empty() && intro.text_metadata.is_empty() {
                                    let mut files = HashMap::new();
                                    for f in &intro.file_metadata {
                                        let name = f.name.clone().unwrap_or_else(|| "file".into());
                                        let dest = store::unique_path(&inbox, &name);
                                        let part = dest.with_file_name(format!(".{}.part", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                                        files.insert(f.payload_id.context("no file payload id")?, Pending { name, size: f.size.unwrap_or(0), dest, part, file: None, got: 0 });
                                    }
                                    incoming = Some(Incoming::Files(files));
                                } else {
                                    ch.send_sharing(&sharing(kind::RESPONSE, |v| v.connection_response = Some(pb::SharingResponseFrame { status: Some(kind::UNSUPPORTED) }))).await?;
                                    ch.disconnect().await?;
                                    break Ok(());
                                }
                                let what = describe(&intro);
                                let (from, pin) = (remote.name.clone(), ch.keys.pin.clone());
                                let mode = host.mode();
                                let tx = consent_tx.clone();
                                tokio::spawn(async move {
                                    let ok = match mode {
                                        Mode::Always => true,
                                        Mode::Off => false,
                                        Mode::Ask => ask("Quick Share", &format!("{from} wants to send {what}\n\nPIN {pin}"), &["Decline", "Accept"]).await.as_deref() == Some("Accept"),
                                    };
                                    let _ = tx.send(ok).await;
                                });
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    if let Some(Incoming::Files(files)) = incoming {
        for p in files.values() {
            let _ = tokio::fs::remove_file(&p.part).await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_matches_reference_algorithm() {
        assert_eq!(pin(&[]), "0000");
        assert_eq!(pin(&[1, 2]), format!("{:04}", 1 + 2 * 31));
        assert_eq!(pin(&[0xFF]), "0001");
    }

    #[test]
    fn endpoint_info_round_trips() {
        let info = endpoint_info("Test Mac", 3);
        let r = parse_endpoint_info(&info).unwrap();
        assert_eq!(r.name, "Test Mac");
        assert_eq!(r.device_type, 3);
    }

    struct TestHost {
        inbox: PathBuf,
        got: Mutex<Vec<String>>,
    }

    impl Host for TestHost {
        fn alias(&self) -> String {
            "server".into()
        }
        fn inbox(&self) -> PathBuf {
            self.inbox.clone()
        }
        fn mode(&self) -> Mode {
            Mode::Always
        }
        fn received(&self, from: &str, path: &Path) {
            self.got.lock().unwrap().push(format!("{from}:{}", path.file_name().unwrap().to_string_lossy()));
        }
        fn text(&self, from: &str, text: &str) {
            self.got.lock().unwrap().push(format!("{from}:{text}"));
        }
    }

    #[tokio::test]
    async fn loopback_text_and_files() {
        let dir = std::env::temp_dir().join(format!("qs-test-{}", std::process::id()));
        let inbox = dir.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(dir.join("big.bin"), &big).unwrap();
        std::fs::write(dir.join("small.txt"), b"hi").unwrap();
        let host = Arc::new(TestHost { inbox: inbox.clone(), got: Mutex::new(Vec::new()) });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let host = host.clone();
            tokio::spawn(async move {
                loop {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let h: Arc<dyn Host> = host.clone();
                    tokio::spawn(async move { serve(h, tcp).await.unwrap() });
                }
            });
        }
        send(addr, "client", Outgoing::Text("hello there".into())).await.unwrap();
        send(addr, "client", Outgoing::Files(vec![dir.join("big.bin"), dir.join("small.txt")])).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut got = host.got.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, ["client:big.bin", "client:hello there", "client:small.txt"]);
        assert_eq!(std::fs::read(inbox.join("big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(inbox.join("small.txt")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coords_strip_sign_byte_and_pad() {
        let mut x = vec![0u8];
        x.extend([0x80; 32]);
        assert_eq!(coord(&x), [0x80; 32]);
        assert_eq!(coord(&[1])[31], 1);
    }
}
