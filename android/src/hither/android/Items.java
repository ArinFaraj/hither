package hither.android;

import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.provider.OpenableColumns;
import android.widget.Toast;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

final class Items {
    static final String CLIP_IMAGE = ".hither-clipboard.png";
    static final String CLIP_TEXT = ".hither-clipboard.txt";

    private Items() {}

    static LocalSend.Item text(String s, String name) {
        LocalSend.Item it = new LocalSend.Item();
        it.name = name;
        it.mime = "text/plain";
        it.text = s;
        return it;
    }

    static LocalSend.Item file(File f, String name, String mime) {
        LocalSend.Item it = new LocalSend.Item();
        it.name = name;
        it.mime = mime;
        it.file = f;
        it.size = f.length();
        return it;
    }

    static LocalSend.Item uri(Context c, Uri u, String fallbackMime) {
        LocalSend.Item it = new LocalSend.Item();
        it.uri = u;
        it.mime = c.getContentResolver().getType(u);
        if (it.mime == null) it.mime = fallbackMime;
        try (Cursor cur = c.getContentResolver().query(u, new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE}, null, null, null)) {
            if (cur != null && cur.moveToFirst()) {
                if (!cur.isNull(0)) it.name = cur.getString(0);
                if (!cur.isNull(1)) it.size = cur.getLong(1);
            }
        } catch (RuntimeException ignored) {
        }
        if (it.name == null) {
            String last = u.getLastPathSegment();
            it.name = last == null || last.isEmpty() ? "file" : last;
        }
        if (it.size < 0) {
            try (InputStream in = c.getContentResolver().openInputStream(u)) {
                long n = 0;
                byte[] buf = new byte[65536];
                int r;
                while (in != null && (r = in.read(buf)) > 0) n += r;
                it.size = n;
            } catch (IOException | RuntimeException e) {
                return null;
            }
        }
        return it;
    }

    static List<LocalSend.Item> fromShare(Context c, Intent i) {
        List<LocalSend.Item> out = new ArrayList<>();
        String type = i.getType();
        List<Uri> uris = new ArrayList<>();
        if (Intent.ACTION_SEND_MULTIPLE.equals(i.getAction())) {
            ArrayList<Uri> l = i.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri.class);
            if (l != null) uris.addAll(l);
        } else {
            Uri u = i.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class);
            if (u != null) uris.add(u);
        }
        if (uris.isEmpty() && i.getClipData() != null) {
            ClipData cd = i.getClipData();
            for (int k = 0; k < cd.getItemCount(); k++) {
                if (cd.getItemAt(k).getUri() != null) uris.add(cd.getItemAt(k).getUri());
            }
        }
        for (Uri u : uris) {
            LocalSend.Item it = uri(c, u, type);
            if (it != null) out.add(it);
        }
        if (out.isEmpty()) {
            CharSequence t = i.getCharSequenceExtra(Intent.EXTRA_TEXT);
            if (t != null && t.length() > 0) out.add(text(t.toString(), "text.txt"));
        }
        return out;
    }

    static List<LocalSend.Item> fromClipboard(Context c, ClipData cd) throws IOException {
        List<LocalSend.Item> out = new ArrayList<>();
        if (cd == null || cd.getItemCount() == 0) return out;
        ClipData.Item item = cd.getItemAt(0);
        Uri u = item.getUri();
        String mime = u == null ? null : c.getContentResolver().getType(u);
        if (u != null && mime != null && mime.startsWith("image/")) {
            File f = new File(ClipProvider.dir(c, "out"), "clip");
            try (InputStream in = c.getContentResolver().openInputStream(u); OutputStream o = new FileOutputStream(f)) {
                if (in == null) throw new IOException("cannot read the clipboard image");
                byte[] buf = new byte[65536];
                int n;
                while ((n = in.read(buf)) > 0) o.write(buf, 0, n);
            }
            out.add(file(f, CLIP_IMAGE, "image/png"));
            return out;
        }
        CharSequence t = item.coerceToText(c);
        if (t != null && t.length() > 0) out.add(text(t.toString(), CLIP_TEXT));
        return out;
    }

    static void sendInBackground(Context c, Peer p, List<LocalSend.Item> items, Runnable after) {
        Context app = c.getApplicationContext();
        Handler main = new Handler(Looper.getMainLooper());
        new Thread(() -> {
            String msg;
            try {
                LocalSend.send(app, p, items, null);
                msg = "Sent to " + p.alias;
            } catch (LocalSend.Rejected e) {
                msg = e.getMessage();
            } catch (IOException e) {
                msg = "Could not reach " + p.alias + ": " + e.getMessage();
            }
            String m = msg;
            main.post(() -> {
                Toast.makeText(app, m, Toast.LENGTH_SHORT).show();
                if (after != null) after.run();
            });
        }).start();
    }
}
