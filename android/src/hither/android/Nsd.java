package hither.android;

import android.content.Context;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.os.Build;
import android.util.Log;

import java.io.IOException;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;

final class Nsd {
    static final String TYPE = "_localsend._tcp";

    private final Context app;
    private final Node node;
    private final NsdManager nsd;
    private NsdManager.RegistrationListener advert;
    private int advertPort;
    private String advertName;
    private NsdManager.DiscoveryListener browse;

    Nsd(Context app, Node node) {
        this.app = app;
        this.node = node;
        this.nsd = app.getSystemService(NsdManager.class);
    }

    synchronized void advertise(int port) {
        String name = Hither.name(app);
        if (advert != null && port == advertPort && name.equals(advertName)) return;
        stopAdvertising();
        NsdServiceInfo s = new NsdServiceInfo();
        s.setServiceName(name);
        s.setServiceType(TYPE);
        s.setPort(port);
        s.setAttribute("fingerprint", Hither.fingerprint(app));
        s.setAttribute("alias", name);
        s.setAttribute("version", "2.1");
        s.setAttribute("protocol", "https");
        s.setAttribute("deviceType", "mobile");
        s.setAttribute("deviceModel", Build.MODEL);
        NsdManager.RegistrationListener l = new NsdManager.RegistrationListener() {
            @Override
            public void onServiceRegistered(NsdServiceInfo s) {
                Log.i("hither", "advertising as " + s.getServiceName());
            }

            @Override
            public void onRegistrationFailed(NsdServiceInfo s, int code) {
                Log.w("hither", "could not advertise (" + code + ")");
                synchronized (Nsd.this) {
                    if (advert == this) advert = null;
                }
            }

            @Override
            public void onServiceUnregistered(NsdServiceInfo s) {
            }

            @Override
            public void onUnregistrationFailed(NsdServiceInfo s, int code) {
            }
        };
        try {
            nsd.registerService(s, NsdManager.PROTOCOL_DNS_SD, l);
            advert = l;
            advertPort = port;
            advertName = name;
        } catch (RuntimeException e) {
            Log.w("hither", "could not advertise", e);
        }
    }

    private void stopAdvertising() {
        if (advert == null) return;
        try {
            nsd.unregisterService(advert);
        } catch (RuntimeException ignored) {
        }
        advert = null;
    }

    synchronized void browse(boolean on) {
        if (on == (browse != null)) return;
        if (!on) {
            try {
                nsd.stopServiceDiscovery(browse);
            } catch (RuntimeException ignored) {
            }
            browse = null;
            return;
        }
        NsdManager.DiscoveryListener l = new NsdManager.DiscoveryListener() {
            @Override
            public void onServiceFound(NsdServiceInfo s) {
                resolve(s);
            }

            @Override
            public void onServiceLost(NsdServiceInfo s) {
            }

            @Override
            public void onDiscoveryStarted(String type) {
            }

            @Override
            public void onDiscoveryStopped(String type) {
            }

            @Override
            public void onStartDiscoveryFailed(String type, int code) {
                Log.w("hither", "could not look for devices (" + code + ")");
                synchronized (Nsd.this) {
                    if (browse == this) browse = null;
                }
            }

            @Override
            public void onStopDiscoveryFailed(String type, int code) {
            }
        };
        try {
            nsd.discoverServices(TYPE, NsdManager.PROTOCOL_DNS_SD, l);
            browse = l;
        } catch (RuntimeException e) {
            Log.w("hither", "could not look for devices", e);
        }
    }

    private void resolve(NsdServiceInfo found) {
        if (Build.VERSION.SDK_INT >= 34) {
            NsdManager.ServiceInfoCallback cb = new NsdManager.ServiceInfoCallback() {
                @Override
                public void onServiceUpdated(NsdServiceInfo s) {
                    try {
                        nsd.unregisterServiceInfoCallback(this);
                    } catch (RuntimeException ignored) {
                    }
                    node.pool.execute(() -> reached(s, s.getHostAddresses()));
                }

                @Override
                public void onServiceInfoCallbackRegistrationFailed(int code) {
                }

                @Override
                public void onServiceLost() {
                }

                @Override
                public void onServiceInfoCallbackUnregistered() {
                }
            };
            try {
                nsd.registerServiceInfoCallback(found, node.pool, cb);
            } catch (RuntimeException e) {
                Log.w("hither", "could not resolve " + found.getServiceName(), e);
            }
            return;
        }
        resolveLegacy(found);
    }

    @SuppressWarnings("deprecation")
    private void resolveLegacy(NsdServiceInfo found) {
        try {
            nsd.resolveService(found, new NsdManager.ResolveListener() {
                @Override
                public void onServiceResolved(NsdServiceInfo s) {
                    List<InetAddress> l = new ArrayList<>();
                    if (s.getHost() != null) l.add(s.getHost());
                    node.pool.execute(() -> reached(s, l));
                }

                @Override
                public void onResolveFailed(NsdServiceInfo s, int code) {
                }
            });
        } catch (RuntimeException ignored) {
        }
    }

    private void reached(NsdServiceInfo s, List<InetAddress> addresses) {
        Map<String, byte[]> attrs = s.getAttributes();
        byte[] raw = attrs.get("fingerprint");
        if (raw == null) return;
        String fp = new String(raw, StandardCharsets.UTF_8).toUpperCase(Locale.ROOT);
        if (fp.isEmpty() || fp.equals(Hither.fingerprint(app))) return;
        byte[] protocol = attrs.get("protocol");
        for (InetAddress a : addresses) {
            if (!(a instanceof Inet4Address)) continue;
            Peer stub = new Peer();
            stub.alias = s.getServiceName();
            stub.fingerprint = fp;
            stub.ip = a.getHostAddress();
            stub.port = s.getPort();
            stub.https = protocol == null || !"http".equals(new String(protocol, StandardCharsets.UTF_8));
            try {
                Peer p = node.remember(LocalSend.register(app, stub, 3000));
                if (p != null) {
                    Log.i("hither", "found " + p.alias + " at " + p.ip + " over DNS-SD");
                    return;
                }
            } catch (IOException | RuntimeException e) {
                Log.i("hither", "DNS-SD found " + stub.alias + " at " + stub.ip + " but it did not answer");
            }
        }
    }
}
