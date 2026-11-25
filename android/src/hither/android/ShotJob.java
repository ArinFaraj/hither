package hither.android;

import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.ComponentName;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.provider.MediaStore;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

public final class ShotJob extends JobService {
    private static final int ID = 1;
    private static final long RECENT_SECONDS = 60;

    static void schedule(Context c) {
        JobInfo job = new JobInfo.Builder(ID, new ComponentName(c, ShotJob.class))
                .addTriggerContentUri(new JobInfo.TriggerContentUri(
                        MediaStore.Images.Media.EXTERNAL_CONTENT_URI, JobInfo.TriggerContentUri.FLAG_NOTIFY_FOR_DESCENDANTS))
                .setTriggerContentUpdateDelay(300)
                .setTriggerContentMaxDelay(2000)
                .build();
        c.getSystemService(JobScheduler.class).schedule(job);
    }

    static void ensure(Context c) {
        if (!Hither.screenshots(c)) return;
        if (c.checkSelfPermission(android.Manifest.permission.READ_MEDIA_IMAGES) != android.content.pm.PackageManager.PERMISSION_GRANTED) return;
        if (c.getSystemService(JobScheduler.class).getPendingJob(ID) == null) schedule(c);
    }

    static void cancel(Context c) {
        c.getSystemService(JobScheduler.class).cancel(ID);
    }

    @Override
    public boolean onStartJob(JobParameters params) {
        Uri[] uris = params.getTriggeredContentUris();
        new Thread(() -> {
            try {
                if (uris != null) {
                    for (int i = uris.length - 1; i >= 0; i--) {
                        LocalSend.Item shot = take(uris[i]);
                        if (shot == null) continue;
                        Peer target = Hither.target(this);
                        if (target != null) {
                            try {
                                Items.send(this, target, java.util.Collections.singletonList(shot));
                            } catch (java.io.IOException e) {
                                android.util.Log.w("hither", "sending the screenshot to " + target.alias + " failed", e);
                            }
                        }
                        break;
                    }
                }
            } finally {
                jobFinished(params, false);
                if (Hither.screenshots(this)) schedule(this);
            }
        }).start();
        return true;
    }

    @Override
    public boolean onStopJob(JobParameters params) {
        return true;
    }

    private LocalSend.Item take(Uri u) {
        String[] cols = {
                MediaStore.Images.Media.RELATIVE_PATH,
                MediaStore.Images.Media.IS_PENDING,
                MediaStore.Images.Media.DATE_ADDED,
                MediaStore.Images.Media.MIME_TYPE,
        };
        String mime;
        try (Cursor c = getContentResolver().query(u, cols, null, null, null)) {
            if (c == null || !c.moveToFirst()) return null;
            String path = c.getString(0);
            if (path == null || !path.toLowerCase().contains("screenshot")) return null;
            if (c.getInt(1) != 0) return null;
            if (System.currentTimeMillis() / 1000 - c.getLong(2) > RECENT_SECONDS) return null;
            mime = c.getString(3);
            if (mime == null || !mime.startsWith("image/")) return null;
        } catch (RuntimeException e) {
            return null;
        }
        File d = ClipProvider.dir(this, "shot");
        String name = ClipProvider.newName();
        File f = new File(d, name);
        try (InputStream in = getContentResolver().openInputStream(u); OutputStream out = new FileOutputStream(f)) {
            if (in == null) return null;
            byte[] buf = new byte[65536];
            int n;
            while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
        } catch (Exception e) {
            f.delete();
            return null;
        }
        ClipProvider.keepNewest(d, 4);
        ClipProvider.setClip(this, ClipProvider.uri("shot", name), "Screenshot");
        return Items.file(f, Items.CLIP_IMAGE, mime);
    }
}
