package hither.android;

import android.content.Context;
import android.net.Uri;
import android.os.Build;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.cert.CertificateException;
import java.security.cert.X509Certificate;
import java.util.List;

import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;

final class LocalSend {
    static final int PORT = 53317;
    private static final String API = "/api/localsend/v2";

    static final class Item {
        String name;
        String mime;
        long size = -1;
        Uri uri;
        File file;
        String text;
    }

    static final class Rejected extends IOException {
        Rejected(String m) {
            super(m);
        }
    }

    private LocalSend() {}

    static JSONObject info(Context c) throws JSONException {
        JSONObject o = new JSONObject();
        o.put("alias", Hither.name(c));
        o.put("version", "2.1");
        o.put("deviceModel", Build.MODEL);
        o.put("deviceType", "mobile");
        o.put("fingerprint", Hither.fingerprint(c));
        int port = Node.get(c).port();
        o.put("port", port > 0 ? port : PORT);
        o.put("protocol", "https");
        o.put("download", false);
        return o;
    }

    static Peer register(Context c, Peer p, int timeout) throws IOException {
        try {
            HttpURLConnection h = open(p, "/register");
            h.setConnectTimeout(timeout);
            h.setReadTimeout(timeout);
            h.setRequestMethod("POST");
            h.setDoOutput(true);
            h.setRequestProperty("Content-Type", "application/json");
            byte[] body = info(c).toString().getBytes(StandardCharsets.UTF_8);
            h.setFixedLengthStreamingMode(body.length);
            try (OutputStream o = h.getOutputStream()) {
                o.write(body);
            }
            if (h.getResponseCode() != 200) throw new IOException(p.alias + " answered " + h.getResponseCode());
            return Peer.fromJson(new JSONObject(readAll(h.getInputStream())), p.ip);
        } catch (JSONException e) {
            throw new IOException(e);
        }
    }

    static boolean probe(Peer p, int timeout) {
        try {
            HttpURLConnection h = open(p, "/info");
            h.setConnectTimeout(timeout);
            h.setReadTimeout(timeout);
            boolean ok = h.getResponseCode() == 200;
            h.disconnect();
            return ok;
        } catch (IOException | RuntimeException e) {
            return false;
        }
    }

    private static HttpURLConnection open(Peer p, String path) throws IOException {
        URL u = new URL((p.https ? "https" : "http") + "://" + p.ip + ":" + p.port + API + path);
        HttpURLConnection c = (HttpURLConnection) u.openConnection();
        if (c instanceof HttpsURLConnection) {
            HttpsURLConnection hc = (HttpsURLConnection) c;
            try {
                SSLContext ctx = SSLContext.getInstance("TLS");
                ctx.init(Identity.get().keyManagers(), new TrustManager[] {new Pinned(p.fingerprint)}, null);
                hc.setSSLSocketFactory(ctx.getSocketFactory());
            } catch (Exception e) {
                throw new IOException(e);
            }
            hc.setHostnameVerifier((host, session) -> true);
        }
        c.setConnectTimeout(4000);
        c.setReadTimeout(120000);
        return c;
    }

    private static final class Pinned implements X509TrustManager {
        private final String want;

        Pinned(String fingerprint) {
            want = fingerprint;
        }

        @Override
        public void checkClientTrusted(X509Certificate[] chain, String auth) throws CertificateException {
            throw new CertificateException("client certs not accepted");
        }

        @Override
        public void checkServerTrusted(X509Certificate[] chain, String auth) throws CertificateException {
            if (chain == null || chain.length == 0) throw new CertificateException("no certificate");
            try {
                byte[] d = MessageDigest.getInstance("SHA-256").digest(chain[0].getEncoded());
                StringBuilder sb = new StringBuilder();
                for (byte b : d) sb.append(String.format("%02X", b));
                if (!want.isEmpty() && !sb.toString().equalsIgnoreCase(want)) throw new CertificateException("certificate does not match the device");
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new CertificateException(e);
            }
        }

        @Override
        public X509Certificate[] getAcceptedIssuers() {
            return new X509Certificate[0];
        }
    }

    private static String readAll(InputStream in) throws IOException {
        if (in == null) return "";
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        byte[] buf = new byte[8192];
        int n;
        while ((n = in.read(buf)) > 0) b.write(buf, 0, n);
        return b.toString("UTF-8");
    }

    private static String q(String s) throws IOException {
        return URLEncoder.encode(s, "UTF-8");
    }

    interface Progress {
        void progress(long done, long total);
    }

    static void send(Context c, Peer p, List<Item> items, Progress progress) throws IOException {
        try {
            JSONObject files = new JSONObject();
            long total = 0;
            for (int i = 0; i < items.size(); i++) {
                Item it = items.get(i);
                JSONObject f = new JSONObject();
                f.put("id", String.valueOf(i));
                f.put("fileName", it.name);
                f.put("fileType", it.mime == null ? "application/octet-stream" : it.mime);
                if (it.text != null) {
                    byte[] b = it.text.getBytes(StandardCharsets.UTF_8);
                    f.put("size", b.length);
                    f.put("preview", it.text);
                } else {
                    f.put("size", it.size);
                    total += it.size;
                }
                files.put(String.valueOf(i), f);
            }
            JSONObject req = new JSONObject();
            req.put("info", info(c));
            req.put("files", files);
            HttpURLConnection h = open(p, "/prepare-upload");
            h.setRequestMethod("POST");
            h.setDoOutput(true);
            h.setRequestProperty("Content-Type", "application/json");
            byte[] body = req.toString().getBytes(StandardCharsets.UTF_8);
            h.setFixedLengthStreamingMode(body.length);
            try (OutputStream o = h.getOutputStream()) {
                o.write(body);
            }
            int code = h.getResponseCode();
            if (code == 204) return;
            if (code == 403) throw new Rejected(p.alias + " declined");
            if (code == 409) throw new Rejected(p.alias + " is busy with another transfer");
            if (code != 200) throw new IOException(p.alias + " answered " + code + " " + readAll(h.getErrorStream()));
            JSONObject resp = new JSONObject(readAll(h.getInputStream()));
            String session = resp.getString("sessionId");
            JSONObject tokens = resp.getJSONObject("files");
            long done = 0;
            for (int i = 0; i < items.size(); i++) {
                String id = String.valueOf(i);
                if (!tokens.has(id)) continue;
                Item it = items.get(i);
                HttpURLConnection u = open(p, "/upload?sessionId=" + q(session) + "&fileId=" + id + "&token=" + q(tokens.getString(id)));
                u.setRequestMethod("POST");
                u.setDoOutput(true);
                u.setRequestProperty("Content-Type", "application/octet-stream");
                byte[] textBytes = it.text == null ? null : it.text.getBytes(StandardCharsets.UTF_8);
                long size = textBytes != null ? textBytes.length : it.size;
                u.setFixedLengthStreamingMode(size);
                try (OutputStream o = u.getOutputStream();
                     InputStream in = textBytes != null ? new java.io.ByteArrayInputStream(textBytes)
                             : it.file != null ? new FileInputStream(it.file) : c.getContentResolver().openInputStream(it.uri)) {
                    if (in == null) throw new IOException("cannot open " + it.name);
                    byte[] buf = new byte[256 * 1024];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        o.write(buf, 0, n);
                        done += n;
                        if (progress != null) progress.progress(done, total);
                    }
                }
                int uc = u.getResponseCode();
                if (uc != 200 && uc != 204) throw new IOException("upload of " + it.name + " failed: " + uc);
            }
        } catch (JSONException e) {
            throw new IOException(e);
        }
    }
}
