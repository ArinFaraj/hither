package hither.android;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.net.Uri;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.MediaStore;
import android.util.Log;
import android.webkit.MimeTypeMap;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.SecureRandom;
import java.security.cert.Certificate;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

import javax.net.ssl.SSLPeerUnverifiedException;
import javax.net.ssl.SSLSocket;

final class Inbox {
    private static final String API = "/api/localsend/v2";
    private static final long IDLE_MS = 120_000;
    private static final long MAX_CLIP = 40L << 20;
    private static final Map<String, BlockingQueue<String>> pending = new ConcurrentHashMap<>();
    private static final SecureRandom random = new SecureRandom();
    private static Session session;

    private static final class Offer {
        String id;
        String name;
        String mime;
        String preview;
        String token;
        long size;
        boolean done;
    }

    private static final class Session {
        String id;
        String ip;
        String from;
        long touched;
        final Map<String, Offer> files = new LinkedHashMap<>();
        final List<Uri> saved = new ArrayList<>();
        final List<String> names = new ArrayList<>();
        final List<String> mimes = new ArrayList<>();
        int done;
    }

    private static final class Result {
        final int code;
        final String json;

        Result(int code, JSONObject json) {
            this.code = code;
            this.json = json == null ? null : json.toString();
        }
    }

    private Inbox() {}

    static void serve(Context c, SSLSocket sock) {
        try (SSLSocket s = sock) {
            s.setSoTimeout(30_000);
            s.startHandshake();
            String certFp = null;
            try {
                Certificate[] chain = s.getSession().getPeerCertificates();
                if (chain.length > 0) certFp = Identity.fingerprint(chain[0].getEncoded());
            } catch (SSLPeerUnverifiedException | GeneralSecurityException ignored) {
            }
            String ip = s.getInetAddress().getHostAddress();
            Http req = Http.read(new BufferedInputStream(s.getInputStream(), 1 << 16));
            if (req == null) return;
            Result r;
            try {
                r = route(c, req, ip, certFp);
            } catch (IOException | JSONException | RuntimeException e) {
                Log.w("hither", "request " + req.path + " from " + ip + " failed", e);
                r = new Result(400, message(String.valueOf(e.getMessage())));
            }
            Http.reply(new BufferedOutputStream(s.getOutputStream()), r.code, r.json);
        } catch (IOException | RuntimeException e) {
            Log.d("hither", "connection dropped", e);
        }
    }

    private static Result route(Context c, Http req, String ip, String certFp) throws IOException, JSONException {
        if (!req.path.startsWith(API)) return new Result(404, null);
        switch (req.method + " " + req.path.substring(API.length())) {
            case "POST /register":
                Node.get(c).remember(new JSONObject(new String(req.bytes(1 << 16), StandardCharsets.UTF_8)), ip);
                return new Result(200, LocalSend.info(c));
            case "GET /info":
                return new Result(200, LocalSend.info(c));
            case "POST /prepare-upload":
                return prepare(c, req, ip, certFp);
            case "POST /upload":
                return upload(c, req, ip);
            case "POST /cancel":
                synchronized (Inbox.class) {
                    if (session != null && session.ip.equals(ip) && session.id.equals(req.query.get("sessionId"))) session = null;
                }
                return new Result(200, null);
            default:
                return new Result(404, null);
        }
    }

    private static String verify(String ip, String certFp, JSONObject info) {
        if (certFp != null) return certFp;
        String fp = info.optString("fingerprint", "");
        if (fp.isEmpty() || "http".equals(info.optString("protocol", "https"))) return null;
        try {
            Peer p = Peer.fromJson(info, ip);
            return LocalSend.probe(p, 3000) ? p.fingerprint : null;
        } catch (JSONException e) {
            return null;
        }
    }

    private static boolean busy() {
        return session != null && System.currentTimeMillis() - session.touched < IDLE_MS;
    }

    private static Result prepare(Context c, Http req, String ip, String certFp) throws IOException, JSONException {
        JSONObject body = new JSONObject(new String(req.bytes(16 << 20), StandardCharsets.UTF_8));
        JSONObject info = body.getJSONObject("info");
        JSONObject files = body.getJSONObject("files");
        String fp = verify(ip, certFp, info);
        String from = info.optString("alias", ip);
        Node.get(c).remember(info, ip);
        synchronized (Inbox.class) {
            if (busy()) return new Result(409, message("Blocked by another session"));
        }
        List<Offer> offers = new ArrayList<>();
        for (Iterator<String> it = files.keys(); it.hasNext(); ) {
            JSONObject f = files.getJSONObject(it.next());
            Offer o = new Offer();
            o.id = f.getString("id");
            o.name = f.getString("fileName");
            o.size = f.getLong("size");
            o.mime = f.optString("fileType", "application/octet-stream");
            o.preview = f.isNull("preview") ? null : f.optString("preview", null);
            if (o.size < 0) throw new IOException("bad size for " + o.name);
            offers.add(o);
        }
        if (offers.isEmpty()) return new Result(400, message("Nothing to receive"));
        boolean trusted = fp != null && Hither.trusted(c, fp);
        Offer first = offers.get(0);
        if (offers.size() == 1 && first.mime.startsWith("text/") && first.preview != null) {
            boolean sync = Items.CLIP_TEXT.equals(first.name);
            if (!trusted && !ask(c, from, fp, "Text: " + shorten(first.preview, 200))) return new Result(403, message("Rejected"));
            copyText(c, first.preview);
            if (!sync) Notes.text(c, from, first.preview);
            return new Result(204, null);
        }
        boolean clipImage = offers.size() == 1 && Items.CLIP_IMAGE.equals(first.name) && first.mime.startsWith("image/");
        if (clipImage && first.size > MAX_CLIP) return new Result(413, message("Image too large"));
        if (!trusted && !ask(c, from, fp, clipImage ? "An image for the clipboard" : describe(c, offers))) {
            return new Result(403, message("Rejected"));
        }
        Session s = new Session();
        s.id = token();
        s.ip = ip;
        s.from = from;
        s.touched = System.currentTimeMillis();
        JSONObject tokens = new JSONObject();
        for (Offer o : offers) {
            o.token = token();
            s.files.put(o.id, o);
            tokens.put(o.id, o.token);
        }
        synchronized (Inbox.class) {
            if (busy()) return new Result(409, message("Blocked by another session"));
            session = s;
        }
        return new Result(200, new JSONObject().put("sessionId", s.id).put("files", tokens));
    }

    private static Result upload(Context c, Http req, String ip) throws IOException {
        String sid = req.query.get("sessionId");
        String fid = req.query.get("fileId");
        String tok = req.query.get("token");
        if (sid == null || fid == null || tok == null) return new Result(400, message("Missing parameters"));
        Session s;
        Offer o;
        synchronized (Inbox.class) {
            s = session;
            if (s == null || !s.id.equals(sid) || !s.ip.equals(ip)) return new Result(403, message("Invalid session"));
            o = s.files.get(fid);
            if (o == null || !o.token.equals(tok) || o.done) return new Result(403, message("Invalid token"));
            s.touched = System.currentTimeMillis();
        }
        PowerManager.WakeLock wake = c.getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "hither:receive");
        wake.setReferenceCounted(false);
        wake.acquire(30 * 60_000L);
        try {
            if (s.files.size() == 1 && Items.CLIP_IMAGE.equals(o.name)) {
                clipImage(c, req.body, o.size);
            } else {
                String mime = mime(o);
                Uri u = save(c, o, mime, req.body, s);
                synchronized (Inbox.class) {
                    s.saved.add(u);
                    s.names.add(o.name);
                    s.mimes.add(mime);
                }
            }
        } catch (IOException | RuntimeException e) {
            synchronized (Inbox.class) {
                if (session == s) session = null;
            }
            throw e;
        } finally {
            wake.release();
        }
        boolean finished;
        synchronized (Inbox.class) {
            o.done = true;
            s.done++;
            s.touched = System.currentTimeMillis();
            finished = s.done >= s.files.size();
            if (finished && session == s) session = null;
        }
        if (finished && !s.saved.isEmpty()) Notes.got(c, s.from, s.saved, s.names, s.mimes);
        return new Result(200, null);
    }

    private static Uri save(Context c, Offer o, String mime, InputStream body, Session s) throws IOException {
        String clean = o.name.replace('\\', '/');
        int slash = clean.lastIndexOf('/');
        StringBuilder dir = new StringBuilder(Environment.DIRECTORY_DOWNLOADS + "/Hither");
        if (slash > 0) {
            for (String seg : clean.substring(0, slash).split("/")) {
                String safe = safe(seg);
                if (!safe.isEmpty()) dir.append('/').append(safe);
            }
        }
        String base = safe(clean.substring(slash + 1));
        if (base.isEmpty()) base = "file";
        ContentResolver cr = c.getContentResolver();
        ContentValues v = new ContentValues();
        v.put(MediaStore.Downloads.DISPLAY_NAME, base);
        v.put(MediaStore.Downloads.MIME_TYPE, mime);
        v.put(MediaStore.Downloads.RELATIVE_PATH, dir.toString());
        v.put(MediaStore.Downloads.IS_PENDING, 1);
        Uri u = cr.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, v);
        if (u == null) throw new IOException("could not create " + base);
        boolean ok = false;
        try (OutputStream out = cr.openOutputStream(u)) {
            if (out == null) throw new IOException("could not write " + base);
            copy(body, out, o.size, s);
            ok = true;
        } finally {
            if (!ok) cr.delete(u, null, null);
        }
        ContentValues done = new ContentValues();
        done.put(MediaStore.Downloads.IS_PENDING, 0);
        cr.update(u, done, null, null);
        return u;
    }

    private static void clipImage(Context c, InputStream body, long size) throws IOException {
        File d = ClipProvider.dir(c, "in");
        String name = ClipProvider.newName();
        File tmp = new File(d, "." + name);
        try (OutputStream out = new FileOutputStream(tmp)) {
            copy(body, out, size, null);
        } catch (IOException e) {
            tmp.delete();
            throw e;
        }
        if (!tmp.renameTo(new File(d, name))) {
            tmp.delete();
            throw new IOException("could not keep the image");
        }
        ClipProvider.keepNewest(d, 4);
        ClipProvider.setClip(c, ClipProvider.uri("in", name), "hither");
    }

    private static void copy(InputStream in, OutputStream out, long size, Session s) throws IOException {
        byte[] buf = new byte[256 * 1024];
        long got = 0;
        long touched = System.currentTimeMillis();
        int n;
        while ((n = in.read(buf)) > 0) {
            got += n;
            if (got > size) throw new IOException("longer than announced");
            out.write(buf, 0, n);
            long now = System.currentTimeMillis();
            if (s != null && now - touched > 5000) {
                touched = now;
                synchronized (Inbox.class) {
                    s.touched = now;
                }
            }
        }
        if (got != size) throw new IOException("cut short at " + got + " of " + size + " bytes");
    }

    static void copyText(Context c, String text) {
        new Handler(Looper.getMainLooper()).post(() -> {
            try {
                c.getSystemService(ClipboardManager.class).setPrimaryClip(ClipData.newPlainText("hither", text));
                Log.i("hither", "clipboard set to " + text.length() + " chars");
            } catch (RuntimeException e) {
                Log.w("hither", "setting the clipboard failed", e);
            }
        });
    }

    private static boolean ask(Context c, String from, String fp, String what) {
        if (!Notes.enabled(c)) return false;
        String id = token();
        BlockingQueue<String> q = new ArrayBlockingQueue<>(1);
        pending.put(id, q);
        try {
            Notes.ask(c, id, from, what, fp != null);
            String a = q.poll(60, TimeUnit.SECONDS);
            if ("always".equals(a)) Hither.trust(c, fp);
            return "accept".equals(a) || "always".equals(a);
        } catch (InterruptedException e) {
            return false;
        } finally {
            pending.remove(id);
            Notes.cancel(c, id.hashCode());
        }
    }

    static void answer(String id, String choice) {
        BlockingQueue<String> q = id == null ? null : pending.get(id);
        if (q != null) q.offer(choice == null ? "decline" : choice);
    }

    private static String mime(Offer o) {
        int dot = o.name.lastIndexOf('.');
        String byExt = dot < 0 ? null : MimeTypeMap.getSingleton().getMimeTypeFromExtension(o.name.substring(dot + 1).toLowerCase(Locale.ROOT));
        if (byExt != null) return byExt;
        return o.mime == null || o.mime.isEmpty() ? "application/octet-stream" : o.mime;
    }

    private static String safe(String s) {
        String t = s.replaceAll("[\\p{Cntrl}:*?\"<>|]", "_").trim();
        return t.equals(".") || t.equals("..") ? "" : t;
    }

    private static String describe(Context c, List<Offer> offers) {
        long total = 0;
        for (Offer o : offers) total += o.size;
        String what = offers.size() == 1 ? offers.get(0).name : offers.size() + " files";
        return what + " (" + android.text.format.Formatter.formatShortFileSize(c, total) + ")";
    }

    private static String shorten(String s, int n) {
        return s.length() <= n ? s : s.substring(0, n) + "...";
    }

    private static JSONObject message(String m) {
        try {
            return new JSONObject().put("message", m);
        } catch (JSONException e) {
            return null;
        }
    }

    private static String token() {
        byte[] b = new byte[16];
        random.nextBytes(b);
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x));
        return sb.toString();
    }
}
