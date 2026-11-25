package hither.android;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.Locale;

final class Peer {
    String alias;
    String fingerprint;
    String ip;
    int port;
    boolean https;
    String type;
    long seen;

    static Peer fromJson(JSONObject o, String ip) throws JSONException {
        Peer p = new Peer();
        p.alias = o.getString("alias");
        p.fingerprint = o.optString("fingerprint", "").toUpperCase(Locale.ROOT);
        p.ip = ip != null ? ip : o.getString("ip");
        p.port = o.optInt("port", LocalSend.PORT);
        p.https = !"http".equals(o.optString("protocol", "https"));
        p.type = o.optString("deviceType", "");
        p.seen = o.optLong("seen", 0);
        return p;
    }

    JSONObject toJson() throws JSONException {
        JSONObject o = new JSONObject();
        o.put("alias", alias);
        o.put("fingerprint", fingerprint);
        o.put("ip", ip);
        o.put("port", port);
        o.put("protocol", https ? "https" : "http");
        o.put("deviceType", type);
        o.put("seen", seen);
        return o;
    }

    @Override
    public String toString() {
        return alias;
    }
}
