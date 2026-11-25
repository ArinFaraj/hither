package hither.android;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.provider.Settings;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Set;

final class Hither {
    private Hither() {}

    static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences("hither", Context.MODE_PRIVATE);
    }

    static String name(Context c) {
        String n = Settings.Global.getString(c.getContentResolver(), Settings.Global.DEVICE_NAME);
        return n == null || n.isEmpty() ? Build.MODEL : n;
    }

    static String fingerprint(Context c) {
        try {
            return Identity.get().fingerprint;
        } catch (java.security.GeneralSecurityException e) {
            throw new IllegalStateException("no LocalSend identity", e);
        }
    }

    static Peer target(Context c) {
        String s = prefs(c).getString("target", null);
        if (s == null) return null;
        try {
            return Peer.fromJson(new JSONObject(s), null);
        } catch (JSONException e) {
            return null;
        }
    }

    static void setTarget(Context c, Peer p) {
        try {
            prefs(c).edit().putString("target", p.toJson().toString()).apply();
        } catch (JSONException ignored) {
        }
    }

    static synchronized boolean trusted(Context c, String fingerprint) {
        return fingerprint != null && prefs(c).getStringSet("trusted", new HashSet<>()).contains(fingerprint.toUpperCase(Locale.ROOT));
    }

    static synchronized void trust(Context c, String fingerprint) {
        if (fingerprint == null || fingerprint.isEmpty()) return;
        Set<String> s = new HashSet<>(prefs(c).getStringSet("trusted", new HashSet<>()));
        if (s.add(fingerprint.toUpperCase(Locale.ROOT))) prefs(c).edit().putStringSet("trusted", s).apply();
    }

    static synchronized List<Peer> known(Context c) {
        List<Peer> out = new ArrayList<>();
        try {
            JSONObject all = new JSONObject(prefs(c).getString("known", "{}"));
            for (Iterator<String> it = all.keys(); it.hasNext(); ) {
                out.add(Peer.fromJson(all.getJSONObject(it.next()), null));
            }
        } catch (JSONException ignored) {
        }
        return out;
    }

    static synchronized void remember(Context c, Peer p) {
        try {
            JSONObject all = new JSONObject(prefs(c).getString("known", "{}"));
            JSONObject old = all.optJSONObject(p.fingerprint);
            if (old != null && p.ip.equals(old.optString("ip")) && p.port == old.optInt("port") && p.alias.equals(old.optString("alias"))) return;
            all.put(p.fingerprint, p.toJson());
            prefs(c).edit().putString("known", all.toString()).apply();
            Peer t = target(c);
            if (t != null && t.fingerprint.equalsIgnoreCase(p.fingerprint)) setTarget(c, p);
        } catch (JSONException ignored) {
        }
    }

    static boolean screenshots(Context c) {
        return prefs(c).getBoolean("screenshots", true);
    }

    static void setScreenshots(Context c, boolean on) {
        prefs(c).edit().putBoolean("screenshots", on).apply();
        if (on) ShotJob.schedule(c);
        else ShotJob.cancel(c);
    }
}
