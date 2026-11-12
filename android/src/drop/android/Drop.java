package drop.android;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.provider.Settings;

import org.json.JSONException;
import org.json.JSONObject;

import java.security.SecureRandom;

final class Drop {
    private Drop() {}

    static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences("drop", Context.MODE_PRIVATE);
    }

    static String name(Context c) {
        String n = Settings.Global.getString(c.getContentResolver(), Settings.Global.DEVICE_NAME);
        return n == null || n.isEmpty() ? Build.MODEL : n;
    }

    static String fingerprint(Context c) {
        SharedPreferences p = prefs(c);
        String fp = p.getString("fingerprint", null);
        if (fp == null) {
            byte[] b = new byte[32];
            new SecureRandom().nextBytes(b);
            StringBuilder sb = new StringBuilder();
            for (byte x : b) sb.append(String.format("%02X", x));
            fp = sb.toString();
            p.edit().putString("fingerprint", fp).apply();
        }
        return fp;
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

    static boolean screenshots(Context c) {
        return prefs(c).getBoolean("screenshots", true);
    }

    static void setScreenshots(Context c, boolean on) {
        prefs(c).edit().putBoolean("screenshots", on).apply();
        if (on) ShotJob.schedule(c);
        else ShotJob.cancel(c);
    }
}
