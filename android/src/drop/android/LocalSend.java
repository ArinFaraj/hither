package drop.android;

import android.content.Context;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.SystemClock;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.HttpURLConnection;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.MulticastSocket;
import java.net.NetworkInterface;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.cert.CertificateException;
import java.security.cert.X509Certificate;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;

final class LocalSend {
    static final int PORT = 53317;
    private static final String GROUP = "224.0.0.167";
    private static final String API = "/api/localsend/v2";

    static final class Item {
        String name;
        String mime;
        long size = -1;
        Uri uri;
        File file;
        String text;
    }

    interface Found {
        void found(Peer p);
    }

    static final class Rejected extends IOException {
        Rejected(String m) {
            super(m);
        }
    }

    private LocalSend() {}

    private static JSONObject info(Context c, int port) throws JSONException {
        JSONObject o = new JSONObject();
        o.put("alias", Drop.name(c));
        o.put("version", "2.1");
        o.put("deviceModel", Build.MODEL);
        o.put("deviceType", "mobile");
        o.put("fingerprint", Drop.fingerprint(c));
        o.put("port", port);
        o.put("protocol", "http");
        o.put("download", false);
        return o;
    }

    private static List<NetworkInterface> lanInterfaces() {
        List<NetworkInterface> out = new ArrayList<>();
        try {
            for (NetworkInterface n : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!n.isUp() || n.isLoopback() || !n.supportsMulticast()) continue;
                for (InetAddress a : Collections.list(n.getInetAddresses())) {
                    if (a instanceof Inet4Address && a.isSiteLocalAddress()) {
                        out.add(n);
                        break;
                    }
                }
            }
        } catch (IOException ignored) {
        }
        return out;
    }

    static void discover(Context c, long millis, Found cb) throws IOException {
        WifiManager wm = (WifiManager) c.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        WifiManager.MulticastLock lock = wm.createMulticastLock("drop");
        lock.setReferenceCounted(false);
        lock.acquire();
        Set<String> seen = Collections.synchronizedSet(new HashSet<>());
        String me = Drop.fingerprint(c);
        ServerSocket server = new ServerSocket(0);
        Thread http = null;
        try (MulticastSocket ms = new MulticastSocket(null)) {
            ms.setReuseAddress(true);
            try {
                ms.bind(new InetSocketAddress(PORT));
            } catch (IOException e) {
                ms.bind(new InetSocketAddress(0));
            }
            InetAddress group = InetAddress.getByName(GROUP);
            List<NetworkInterface> ifaces = lanInterfaces();
            for (NetworkInterface n : ifaces) {
                try {
                    ms.joinGroup(new InetSocketAddress(group, PORT), n);
                } catch (IOException ignored) {
                }
            }
            ms.setSoTimeout(150);
            final JSONObject mine = info(c, server.getLocalPort());
            http = new Thread(() -> serveRegister(server, mine, me, seen, cb));
            http.start();
            byte[] hello = new JSONObject(mine.toString()).put("announce", true).toString().getBytes(StandardCharsets.UTF_8);
            long start = SystemClock.uptimeMillis();
            long[] at = {0, 300, 1200};
            int sent = 0;
            byte[] buf = new byte[65536];
            while (SystemClock.uptimeMillis() - start < millis) {
                if (sent < at.length && SystemClock.uptimeMillis() - start >= at[sent]) {
                    for (NetworkInterface n : ifaces) {
                        try {
                            ms.setNetworkInterface(n);
                            ms.send(new DatagramPacket(hello, hello.length, group, PORT));
                        } catch (IOException ignored) {
                        }
                    }
                    sent++;
                }
                DatagramPacket p = new DatagramPacket(buf, buf.length);
                try {
                    ms.receive(p);
                } catch (SocketTimeoutException e) {
                    continue;
                }
                found(new String(p.getData(), 0, p.getLength(), StandardCharsets.UTF_8), p.getAddress().getHostAddress(), me, seen, cb);
            }
        } catch (JSONException e) {
            throw new IOException(e);
        } finally {
            server.close();
            if (http != null) {
                try {
                    http.join(500);
                } catch (InterruptedException ignored) {
                }
            }
            lock.release();
        }
    }

    private static void found(String json, String ip, String me, Set<String> seen, Found cb) {
        try {
            JSONObject o = new JSONObject(json);
            String fp = o.optString("fingerprint", "");
            if (fp.isEmpty() || fp.equalsIgnoreCase(me) || !seen.add(fp)) return;
            cb.found(Peer.fromJson(o, ip));
        } catch (JSONException ignored) {
        }
    }

    private static void serveRegister(ServerSocket server, JSONObject mine, String me, Set<String> seen, Found cb) {
        while (!server.isClosed()) {
            try (Socket s = server.accept()) {
                s.setSoTimeout(3000);
                InputStream in = s.getInputStream();
                BufferedReader r = new BufferedReader(new InputStreamReader(in, StandardCharsets.UTF_8));
                String line = r.readLine();
                int len = 0;
                String h;
                while ((h = r.readLine()) != null && !h.isEmpty()) {
                    int i = h.indexOf(':');
                    if (i > 0 && h.substring(0, i).trim().equalsIgnoreCase("content-length")) {
                        len = Integer.parseInt(h.substring(i + 1).trim());
                    }
                }
                if (len > 65536) len = 65536;
                char[] body = new char[len];
                int got = 0;
                while (got < len) {
                    int n = r.read(body, got, len - got);
                    if (n < 0) break;
                    got += n;
                }
                boolean ok = line != null && line.startsWith("POST ") && line.contains(API + "/register");
                if (ok) found(new String(body, 0, got), s.getInetAddress().getHostAddress(), me, seen, cb);
                byte[] reply = ok ? mine.toString().getBytes(StandardCharsets.UTF_8) : new byte[0];
                String head = (ok ? "HTTP/1.1 200 OK" : "HTTP/1.1 404 Not Found") + "\r\nContent-Type: application/json\r\nContent-Length: "
                        + reply.length + "\r\nConnection: close\r\n\r\n";
                OutputStream out = s.getOutputStream();
                out.write(head.getBytes(StandardCharsets.UTF_8));
                out.write(reply);
                out.flush();
            } catch (IOException | RuntimeException ignored) {
            }
        }
    }

    private static HttpURLConnection open(Peer p, String path) throws IOException {
        URL u = new URL((p.https ? "https" : "http") + "://" + p.ip + ":" + p.port + API + path);
        HttpURLConnection c = (HttpURLConnection) u.openConnection();
        if (c instanceof HttpsURLConnection) {
            HttpsURLConnection hc = (HttpsURLConnection) c;
            try {
                SSLContext ctx = SSLContext.getInstance("TLS");
                ctx.init(null, new TrustManager[] {new Pinned(p.fingerprint)}, null);
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
                if (!sb.toString().equalsIgnoreCase(want)) throw new CertificateException("certificate does not match the device");
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
            req.put("info", info(c, PORT));
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
