package hither.android;

import android.app.DownloadManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;

import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;

final class Notes {
    static final int READY_ID = 1;
    private static final String READY = "ready";
    private static final String ASK = "ask";
    private static final String GOT = "got";
    private static final AtomicInteger next = new AtomicInteger(100);

    private Notes() {}

    static void channels(Context c) {
        NotificationManager nm = c.getSystemService(NotificationManager.class);
        NotificationChannel ready = new NotificationChannel(READY, "Ready to receive", NotificationManager.IMPORTANCE_MIN);
        ready.setShowBadge(false);
        nm.createNotificationChannel(ready);
        nm.createNotificationChannel(new NotificationChannel(ASK, "Requests to send", NotificationManager.IMPORTANCE_HIGH));
        nm.createNotificationChannel(new NotificationChannel(GOT, "Received", NotificationManager.IMPORTANCE_DEFAULT));
    }

    static boolean enabled(Context c) {
        return c.getSystemService(NotificationManager.class).areNotificationsEnabled();
    }

    static Notification ready(Context c) {
        return new Notification.Builder(c, READY)
                .setSmallIcon(R.drawable.ic_tile)
                .setContentTitle("Ready to receive")
                .setContentText("Other devices see this phone as " + Hither.name(c))
                .setContentIntent(open(c))
                .setOngoing(true)
                .setShowWhen(false)
                .build();
    }

    static void ask(Context c, String id, String from, String what, boolean canTrust) {
        Notification.Builder b = new Notification.Builder(c, ASK)
                .setSmallIcon(R.drawable.ic_tile)
                .setContentTitle(from + " wants to send you")
                .setContentText(what)
                .setStyle(new Notification.BigTextStyle().bigText(what))
                .setCategory(Notification.CATEGORY_MESSAGE)
                .setTimeoutAfter(60_000)
                .setAutoCancel(true)
                .addAction(action(c, id, "decline", "Decline"))
                .addAction(action(c, id, "accept", "Accept"));
        if (canTrust) b.addAction(action(c, id, "always", "Always accept"));
        post(c, id.hashCode(), b.build());
    }

    static void cancel(Context c, int id) {
        c.getSystemService(NotificationManager.class).cancel(id);
    }

    static void got(Context c, String from, List<Uri> uris, List<String> names, List<String> mimes) {
        Intent view;
        String title;
        if (uris.size() == 1) {
            view = new Intent(Intent.ACTION_VIEW).setDataAndType(uris.get(0), mimes.get(0)).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            title = names.get(0);
        } else {
            view = new Intent(DownloadManager.ACTION_VIEW_DOWNLOADS);
            title = uris.size() + " files";
        }
        view.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        int id = next.getAndIncrement();
        post(c, id, new Notification.Builder(c, GOT)
                .setSmallIcon(R.drawable.ic_tile)
                .setContentTitle(title)
                .setContentText("From " + from + ", saved in Downloads/Hither")
                .setContentIntent(PendingIntent.getActivity(c, id, view, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT))
                .setAutoCancel(true)
                .build());
    }

    static void text(Context c, String from, String text) {
        post(c, next.getAndIncrement(), new Notification.Builder(c, GOT)
                .setSmallIcon(R.drawable.ic_tile)
                .setContentTitle(from + " sent text, copied")
                .setContentText(text)
                .setStyle(new Notification.BigTextStyle().bigText(text))
                .setAutoCancel(true)
                .build());
    }

    private static PendingIntent open(Context c) {
        return PendingIntent.getActivity(c, 0, new Intent(c, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
    }

    private static Notification.Action action(Context c, String id, String choice, String label) {
        Intent i = new Intent(c, Answer.class).putExtra("id", id).putExtra("choice", choice);
        PendingIntent pi = PendingIntent.getBroadcast(c, (id + choice).hashCode(), i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Action.Builder(null, label, pi).build();
    }

    private static void post(Context c, int id, Notification n) {
        try {
            c.getSystemService(NotificationManager.class).notify(id, n);
        } catch (SecurityException ignored) {
        }
    }
}
