package hither.android;

import android.content.Context;
import android.net.wifi.WifiManager;
import android.os.SystemClock;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.IOException;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.StandardProtocolFamily;
import java.net.StandardSocketOptions;
import java.nio.ByteBuffer;
import java.nio.channels.DatagramChannel;
import java.nio.channels.MembershipKey;
import java.net.NetworkInterface;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.cert.X509Certificate;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CopyOnWriteArraySet;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLServerSocket;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;

final class Node {
    interface Listener {
        void changed();
    }

    private static final String GROUP = "224.0.0.167";
    private static final int[] PORTS = {LocalSend.PORT, 53318, 53319, 0};
    private static final long FRESH_MS = 10 * 60_000;
    private static Node node;

    final ExecutorService pool = Executors.newCachedThreadPool();
    private final Context app;
    private final Map<String, Peer> peers = new ConcurrentHashMap<>();
    private final Set<Listener> listeners = new CopyOnWriteArraySet<>();
    private final Object sending = new Object();
    private final WifiManager.MulticastLock screenLock;
    private final WifiManager.MulticastLock pickLock;
    private SSLServerSocket server;
    private DatagramChannel udp;
    private final Map<String, MembershipKey> joined = new HashMap<>();
    private volatile int port;
    private long lastAnnounce = -60_000;
    private String lastAddress;
    private long lastScan = -3_600_000;
    private String lastScanned;

    private Node(Context app) {
        this.app = app;
        WifiManager wm = app.getSystemService(WifiManager.class);
        screenLock = wm.createMulticastLock("hither-screen");
        screenLock.setReferenceCounted(false);
        pickLock = wm.createMulticastLock("hither-pick");
        pickLock.setReferenceCounted(false);
        for (Peer p : Hither.known(app)) peers.put(p.fingerprint, p);
    }

    static synchronized Node get(Context c) {
        if (node == null) node = new Node(c.getApplicationContext());
        return node;
    }

    int port() {
        return port;
    }

    boolean running() {
        SSLServerSocket s = server;
        return s != null && !s.isClosed();
    }

    synchronized void start() throws IOException, GeneralSecurityException {
        if (running()) return;
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(Identity.get().keyManagers(), new TrustManager[] {new AnyClient()}, null);
        SSLServerSocket s = bind(ctx);
        s.setWantClientAuth(true);
        server = s;
        port = s.getLocalPort();
        pool.execute(() -> accept(s));
        if (udp == null) {
            DatagramChannel m = DatagramChannel.open(StandardProtocolFamily.INET);
            m.setOption(StandardSocketOptions.SO_REUSEADDR, true);
            m.bind(new InetSocketAddress(InetAddress.getByAddress(new byte[4]), LocalSend.PORT));
            m.setOption(StandardSocketOptions.IP_MULTICAST_TTL, 1);
            udp = m;
            pool.execute(() -> listen(m));
        }
        rejoin();
        Log.i("hither", "receiving on port " + port);
    }

    void startQuietly() {
        try {
            start();
        } catch (IOException | GeneralSecurityException | RuntimeException e) {
            Log.w("hither", "could not start receiving", e);
        }
    }

    private static SSLServerSocket bind(SSLContext ctx) throws IOException {
        IOException last = null;
        for (int p : PORTS) {
            SSLServerSocket s = (SSLServerSocket) ctx.getServerSocketFactory().createServerSocket();
            try {
                s.setReuseAddress(true);
                s.bind(new InetSocketAddress(p), 50);
                return s;
            } catch (IOException e) {
                last = e;
                s.close();
            }
        }
        throw last;
    }

    private void accept(SSLServerSocket s) {
        while (!s.isClosed()) {
            try {
                SSLSocket c = (SSLSocket) s.accept();
                pool.execute(() -> Inbox.serve(app, c));
            } catch (IOException e) {
                if (s.isClosed()) return;
                SystemClock.sleep(1000);
            }
        }
    }

    private void listen(DatagramChannel m) {
        ByteBuffer buf = ByteBuffer.allocate(65536);
        while (m.isOpen()) {
            buf.clear();
            InetSocketAddress from;
            try {
                from = (InetSocketAddress) m.receive(buf);
            } catch (IOException e) {
                if (!m.isOpen()) return;
                SystemClock.sleep(1000);
                continue;
            }
            if (from == null) continue;
            buf.flip();
            JSONObject info;
            try {
                info = new JSONObject(StandardCharsets.UTF_8.decode(buf).toString());
            } catch (JSONException e) {
                continue;
            }
            Peer peer = remember(info, from.getAddress().getHostAddress());
            if (peer != null && (info.optBoolean("announce", false) || info.optBoolean("announcement", false))) {
                pool.execute(() -> answer(peer));
            }
        }
    }

    private void answer(Peer p) {
        try {
            remember(LocalSend.register(app, p, 3000));
        } catch (IOException | RuntimeException e) {
            multicast(false);
        }
    }

    Peer remember(JSONObject info, String ip) {
        String fp = info.optString("fingerprint", "").toUpperCase(Locale.ROOT);
        if (fp.isEmpty() || fp.equals(Hither.fingerprint(app))) return null;
        try {
            return remember(Peer.fromJson(info, ip));
        } catch (JSONException e) {
            return null;
        }
    }

    Peer remember(Peer p) {
        if (p.fingerprint.isEmpty() || p.fingerprint.equals(Hither.fingerprint(app))) return null;
        p.seen = System.currentTimeMillis();
        peers.put(p.fingerprint, p);
        if (Hither.trusted(app, p.fingerprint)) Hither.remember(app, p);
        for (Listener l : listeners) l.changed();
        return p;
    }

    Peer peer(String fingerprint) {
        return fingerprint == null ? null : peers.get(fingerprint.toUpperCase(Locale.ROOT));
    }

    List<Peer> peers() {
        long now = System.currentTimeMillis();
        List<Peer> out = new ArrayList<>();
        for (Peer p : peers.values()) {
            if (now - p.seen < FRESH_MS || Hither.trusted(app, p.fingerprint)) out.add(p);
        }
        Collections.sort(out, (a, b) -> a.alias.compareToIgnoreCase(b.alias));
        return out;
    }

    Peer refresh(Peer p, long waitMs) {
        long before = System.currentTimeMillis();
        announce(true);
        pool.execute(() -> scan(60_000));
        long end = SystemClock.elapsedRealtime() + waitMs;
        while (SystemClock.elapsedRealtime() < end) {
            Peer q = peer(p.fingerprint);
            if (q != null && q.seen >= before) return q;
            SystemClock.sleep(150);
        }
        return null;
    }

    void addListener(Listener l) {
        listeners.add(l);
    }

    void removeListener(Listener l) {
        listeners.remove(l);
    }

    void announce(boolean force) {
        String address = lanAddress();
        if (address == null) return;
        synchronized (this) {
            long now = SystemClock.elapsedRealtime();
            boolean moved = address != null && !address.equals(lastAddress);
            if (!force && !moved && now - lastAnnounce < 20_000) return;
            lastAnnounce = now;
            lastAddress = address;
        }
        pool.execute(() -> {
            multicast(true);
            List<Peer> known = Hither.known(app);
            java.util.concurrent.atomic.AtomicInteger answered = new java.util.concurrent.atomic.AtomicInteger();
            java.util.concurrent.CountDownLatch done = new java.util.concurrent.CountDownLatch(known.size());
            for (Peer p : known) {
                pool.execute(() -> {
                    try {
                        if (!p.ip.isEmpty() && remember(LocalSend.register(app, p, 3000)) != null) answered.incrementAndGet();
                    } catch (IOException | RuntimeException ignored) {
                    } finally {
                        done.countDown();
                    }
                });
            }
            SystemClock.sleep(400);
            multicast(true);
            try {
                done.await(4, java.util.concurrent.TimeUnit.SECONDS);
            } catch (InterruptedException ignored) {
            }
            if (!known.isEmpty() && answered.get() == 0) scan(60 * 60_000);
        });
    }

    void scan(long every) {
        String me = lanAddress();
        if (me == null) return;
        synchronized (this) {
            long now = SystemClock.elapsedRealtime();
            if (me.equals(lastScanned) && now - lastScan < every) return;
            lastScan = now;
            lastScanned = me;
        }
        String base = me.substring(0, me.lastIndexOf('.') + 1);
        ExecutorService ex = Executors.newFixedThreadPool(48);
        for (int i = 1; i < 255; i++) {
            String ip = base + i;
            if (!ip.equals(me)) ex.execute(() -> probe(ip));
        }
        ex.shutdown();
    }

    private void probe(String ip) {
        for (int p : new int[] {LocalSend.PORT, 53318}) {
            try (java.net.Socket s = new java.net.Socket()) {
                s.connect(new InetSocketAddress(ip, p), 600);
            } catch (IOException e) {
                continue;
            }
            Peer stub = new Peer();
            stub.alias = ip;
            stub.fingerprint = "";
            stub.ip = ip;
            stub.port = p;
            for (boolean https : new boolean[] {true, false}) {
                stub.https = https;
                try {
                    remember(LocalSend.register(app, stub, 2000));
                    break;
                } catch (IOException | RuntimeException ignored) {
                }
            }
        }
    }

    private void multicast(boolean announce) {
        DatagramChannel m = udp;
        if (m == null) return;
        byte[] msg;
        InetAddress group;
        try {
            msg = LocalSend.info(app).put("announce", announce).put("announcement", announce).toString().getBytes(StandardCharsets.UTF_8);
            group = InetAddress.getByName(GROUP);
        } catch (JSONException | IOException e) {
            return;
        }
        synchronized (sending) {
            for (NetworkInterface n : lanInterfaces()) {
                try {
                    m.setOption(StandardSocketOptions.IP_MULTICAST_IF, n);
                    m.send(ByteBuffer.wrap(msg), new InetSocketAddress(group, LocalSend.PORT));
                } catch (IOException ignored) {
                }
            }
        }
    }

    synchronized void rejoin() {
        DatagramChannel m = udp;
        if (m == null) return;
        InetAddress g;
        try {
            g = InetAddress.getByName(GROUP);
        } catch (IOException e) {
            return;
        }
        for (MembershipKey k : joined.values()) k.drop();
        joined.clear();
        for (NetworkInterface n : lanInterfaces()) {
            try {
                joined.put(n.getName(), m.join(g, n));
            } catch (IOException | RuntimeException e) {
                Log.w("hither", "could not listen for devices on " + n.getName(), e);
            }
        }
    }

    void screen(boolean on) {
        if (on) screenLock.acquire();
        else if (screenLock.isHeld()) screenLock.release();
    }

    void picking(boolean on) {
        if (on) pickLock.acquire();
        else if (pickLock.isHeld()) pickLock.release();
    }

    static List<NetworkInterface> lanInterfaces() {
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
        } catch (IOException | NullPointerException ignored) {
        }
        return out;
    }

    static String lanAddress() {
        for (NetworkInterface n : lanInterfaces()) {
            for (InetAddress a : Collections.list(n.getInetAddresses())) {
                if (a instanceof Inet4Address && a.isSiteLocalAddress()) return a.getHostAddress();
            }
        }
        return null;
    }

    private static final class AnyClient implements X509TrustManager {
        @Override
        public void checkClientTrusted(X509Certificate[] chain, String auth) {
        }

        @Override
        public void checkServerTrusted(X509Certificate[] chain, String auth) throws java.security.cert.CertificateException {
            throw new java.security.cert.CertificateException("not a client");
        }

        @Override
        public X509Certificate[] getAcceptedIssuers() {
            return new X509Certificate[0];
        }
    }
}
