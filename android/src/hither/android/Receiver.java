package hither.android;

import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;

public final class Receiver extends Service {
    private ConnectivityManager.NetworkCallback net;
    private BroadcastReceiver screen;

    static void start(Context c) {
        try {
            c.startForegroundService(new Intent(c, Receiver.class));
        } catch (RuntimeException e) {
            Log.w("hither", "could not start receiving in the background", e);
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        Notes.channels(this);
        startForeground(Notes.READY_ID, Notes.ready(this), ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE);
        Node node = Node.get(this);
        node.pool.execute(() -> {
            node.startQuietly();
            node.announce(true);
        });
        net = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onLinkPropertiesChanged(Network n, LinkProperties lp) {
                node.pool.execute(() -> {
                    node.startQuietly();
                    node.rejoin();
                    node.announce(false);
                });
            }
        };
        getSystemService(ConnectivityManager.class).registerNetworkCallback(new NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET)
                .build(), net);
        screen = new BroadcastReceiver() {
            @Override
            public void onReceive(Context c, Intent i) {
                boolean on = Intent.ACTION_SCREEN_ON.equals(i.getAction());
                node.screen(on);
                if (on) node.announce(false);
            }
        };
        IntentFilter f = new IntentFilter(Intent.ACTION_SCREEN_ON);
        f.addAction(Intent.ACTION_SCREEN_OFF);
        registerReceiver(screen, f);
        node.screen(getSystemService(PowerManager.class).isInteractive());
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onDestroy() {
        if (net != null) getSystemService(ConnectivityManager.class).unregisterNetworkCallback(net);
        if (screen != null) unregisterReceiver(screen);
        Node.get(this).screen(false);
        super.onDestroy();
    }
}
