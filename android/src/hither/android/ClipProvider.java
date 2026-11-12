package hither.android;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Binder;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;

import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

public final class ClipProvider extends ContentProvider {
    static final String AUTHORITY = "hither.android.clip";
    private static final long MAX = 40L << 20;
    private static final int SHELL_UID = 2000;

    static File dir(Context c, String kind) {
        File d = new File(c.getCacheDir(), kind);
        d.mkdirs();
        return d;
    }

    static Uri uri(String kind, String name) {
        return Uri.parse("content://" + AUTHORITY + "/" + kind + "/" + name);
    }

    static void keepNewest(File d, int n) {
        File[] fs = d.listFiles();
        if (fs == null || fs.length <= n) return;
        java.util.Arrays.sort(fs, (a, b) -> Long.compare(b.lastModified(), a.lastModified()));
        for (int i = n; i < fs.length; i++) fs[i].delete();
    }

    static void setClip(Context c, Uri u, String label) {
        new Handler(Looper.getMainLooper()).post(() -> {
            ClipboardManager cm = c.getSystemService(ClipboardManager.class);
            try {
                cm.setPrimaryClip(ClipData.newUri(c.getContentResolver(), label, u));
                android.util.Log.i("hither", "clipboard set to " + u);
            } catch (RuntimeException e) {
                android.util.Log.w("hither", "setting the clipboard failed", e);
            }
        });
    }

    @Override
    public boolean onCreate() {
        ShotJob.ensure(getContext());
        return true;
    }

    static String newName() {
        byte[] b = new byte[16];
        new java.security.SecureRandom().nextBytes(b);
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x));
        return sb.append(".png").toString();
    }

    private File file(Uri u) throws FileNotFoundException {
        java.util.List<String> seg = u.getPathSegments();
        if (seg.size() != 2 || !(seg.get(0).equals("in") || seg.get(0).equals("shot"))) throw new FileNotFoundException(u.toString());
        String name = seg.get(1);
        if (name.contains("/") || name.startsWith(".")) throw new FileNotFoundException(u.toString());
        File f = new File(dir(getContext(), seg.get(0)), name);
        if (!f.isFile()) throw new FileNotFoundException(u.toString());
        return f;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri u, String mode) throws FileNotFoundException {
        if (mode.contains("w")) return openWrite(u);
        return ParcelFileDescriptor.open(file(u), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    private ParcelFileDescriptor openWrite(Uri u) throws FileNotFoundException {
        int uid = Binder.getCallingUid();
        if (uid != SHELL_UID && uid != 0) throw new SecurityException("only adb may set the clipboard");
        if (!"/in".equals(u.getPath())) throw new FileNotFoundException(u.toString());
        Context c = getContext();
        ParcelFileDescriptor[] pipe;
        try {
            pipe = ParcelFileDescriptor.createPipe();
        } catch (IOException e) {
            throw new FileNotFoundException(e.getMessage());
        }
        ParcelFileDescriptor read = pipe[0];
        new Thread(() -> {
            File d = dir(c, "in");
            String name = newName();
            File tmp = new File(d, "." + name);
            boolean ok = false;
            try (InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(read);
                 OutputStream out = new FileOutputStream(tmp)) {
                byte[] buf = new byte[65536];
                long total = 0;
                int n;
                while ((n = in.read(buf)) > 0) {
                    total += n;
                    if (total > MAX) throw new IOException("too large");
                    out.write(buf, 0, n);
                }
                ok = total > 0;
                android.util.Log.i("hither", "clip in: " + total + " bytes");
            } catch (IOException e) {
                android.util.Log.w("hither", "clip in failed", e);
            }
            if (!ok || !tmp.renameTo(new File(d, name))) {
                android.util.Log.w("hither", "clip in: nothing saved");
                tmp.delete();
                return;
            }
            keepNewest(d, 4);
            setClip(c, uri("in", name), "hither");
        }).start();
        return pipe[1];
    }

    @Override
    public String getType(Uri u) {
        return "image/png";
    }

    @Override
    public Cursor query(Uri u, String[] projection, String selection, String[] args, String sort) {
        File f;
        try {
            f = file(u);
        } catch (FileNotFoundException | SecurityException e) {
            return null;
        }
        String[] cols = projection != null ? projection : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor m = new MatrixCursor(cols, 1);
        Object[] row = new Object[cols.length];
        for (int i = 0; i < cols.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(cols[i])) row[i] = f.getName();
            else if (OpenableColumns.SIZE.equals(cols[i])) row[i] = f.length();
        }
        m.addRow(row);
        return m;
    }

    @Override
    public Uri insert(Uri u, ContentValues v) {
        return null;
    }

    @Override
    public int delete(Uri u, String s, String[] a) {
        return 0;
    }

    @Override
    public int update(Uri u, ContentValues v, String s, String[] a) {
        return 0;
    }
}
