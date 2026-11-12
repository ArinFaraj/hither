package hither;

import android.content.ClipData;
import android.content.IOnPrimaryClipChangedListener;
import android.net.Uri;
import android.os.IBinder;
import android.os.SystemClock;
import android.util.Base64;
import android.view.InputEvent;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

public final class Helper {
    private static final String PKG = "com.android.shell";
    private static Object clipboard;
    private static Object input;
    private static Method inject;
    private static volatile String lastSet;
    private static volatile String lastImage;
    private static final String OWN = "hither.android.clip";
    private static final PrintStream out = new PrintStream(System.out, true);

    public static void main(String[] args) throws Exception {
        IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, "clipboard");
        clipboard = Class.forName("android.content.IClipboard$Stub")
                .getMethod("asInterface", IBinder.class).invoke(null, binder);
        boolean listening = listen();
        say("R " + (listening ? "listen" : "nolisten"));

        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        String line;
        while ((line = in.readLine()) != null) {
            try {
                handle(line);
            } catch (Throwable t) {
                say("E " + b64(String.valueOf(t)));
            }
        }
        System.exit(0);
    }

    private static void handle(String line) throws Exception {
        if (line.isEmpty()) return;
        char op = line.charAt(0);
        String arg = line.length() > 2 ? unb64(line.substring(2)) : "";
        switch (op) {
            case 'S':
                setClip(arg);
                say("K");
                break;
            case 'G':
                String t = getClip();
                say("V " + (t == null ? "" : b64(t)));
                break;
            case 'T':
                typeText(arg);
                say("K");
                break;
            case 'K':
                for (String k : arg.split(" ")) key(Integer.parseInt(k.trim()));
                say("K");
                break;
            case 'P':
                say("K");
                break;
            default:
                say("E " + b64("unknown op"));
        }
    }

    private static synchronized void say(String s) {
        out.print(s);
        out.print('\n');
        out.flush();
    }

    private static String b64(String s) {
        return Base64.encodeToString(s.getBytes(StandardCharsets.UTF_8), Base64.NO_WRAP);
    }

    private static String unb64(String s) {
        return new String(Base64.decode(s, Base64.DEFAULT), StandardCharsets.UTF_8);
    }

    private static Object[] fill(Method m, Object first) {
        Class<?>[] types = m.getParameterTypes();
        Object[] a = new Object[types.length];
        int strings = 0;
        for (int i = 0; i < types.length; i++) {
            Class<?> c = types[i];
            if (i == 0 && first != null) {
                a[i] = first;
            } else if (c == String.class) {
                a[i] = strings++ == 0 ? PKG : null;
            } else if (c == int.class) {
                a[i] = 0;
            } else if (c == boolean.class) {
                a[i] = false;
            } else {
                a[i] = null;
            }
        }
        return a;
    }

    private static Object call(String name, Object first) throws Exception {
        Exception last = null;
        for (Method m : clipboard.getClass().getMethods()) {
            if (!m.getName().equals(name)) continue;
            Class<?>[] types = m.getParameterTypes();
            if (first != null && (types.length == 0 || !types[0].isInstance(first))) continue;
            try {
                return m.invoke(clipboard, fill(m, first));
            } catch (Exception e) {
                last = e;
            }
        }
        if (last != null) throw last;
        throw new NoSuchMethodException(name);
    }

    private static String getClip() throws Exception {
        ClipData clip = (ClipData) call("getPrimaryClip", null);
        if (clip == null || clip.getItemCount() == 0) return null;
        CharSequence t = clip.getItemAt(0).getText();
        return t == null ? null : t.toString();
    }

    private static void setClip(String text) throws Exception {
        lastSet = text;
        call("setPrimaryClip", ClipData.newPlainText("hither", text));
    }

    private static String imageUri(ClipData clip) {
        if (clip == null || clip.getItemCount() == 0 || clip.getDescription() == null) return null;
        if (!clip.getDescription().hasMimeType("image/*")) return null;
        Uri u = clip.getItemAt(0).getUri();
        if (u == null || (OWN.equals(u.getAuthority()) && u.getPath() != null && u.getPath().startsWith("/in"))) return null;
        return u.toString();
    }

    private static boolean listen() {
        IOnPrimaryClipChangedListener l = new IOnPrimaryClipChangedListener.Stub() {
            @Override
            public void dispatchPrimaryClipChanged() {
                try {
                    ClipData clip = (ClipData) call("getPrimaryClip", null);
                    String img = imageUri(clip);
                    if (img != null) {
                        if (img.equals(lastImage)) return;
                        lastImage = img;
                        lastSet = null;
                        say("I " + b64(img));
                        return;
                    }
                    lastImage = null;
                    if (clip == null || clip.getItemCount() == 0) return;
                    CharSequence cs = clip.getItemAt(0).getText();
                    String t = cs == null ? null : cs.toString();
                    if (t == null || t.equals(lastSet)) return;
                    lastSet = t;
                    say("C " + b64(t));
                } catch (Throwable e) {
                    say("E " + b64(String.valueOf(e)));
                }
            }
        };
        try {
            call("addPrimaryClipChangedListener", l);
            return true;
        } catch (Throwable e) {
            say("E " + b64(String.valueOf(e)));
            return false;
        }
    }

    private static void initInput() throws Exception {
        if (inject != null) return;
        try {
            Class<?> g = Class.forName("android.hardware.input.InputManagerGlobal");
            input = g.getMethod("getInstance").invoke(null);
        } catch (ClassNotFoundException e) {
            Class<?> im = Class.forName("android.hardware.input.InputManager");
            input = im.getMethod("getInstance").invoke(null);
        }
        inject = input.getClass().getMethod("injectInputEvent", InputEvent.class, int.class);
    }

    private static void send(KeyEvent e) throws Exception {
        initInput();
        inject.invoke(input, e, 0);
    }

    private static void key(int code) throws Exception {
        long now = SystemClock.uptimeMillis();
        send(new KeyEvent(now, now, KeyEvent.ACTION_DOWN, code, 0));
        send(new KeyEvent(now, now, KeyEvent.ACTION_UP, code, 0));
    }

    private static void typeText(String text) throws Exception {
        KeyCharacterMap map = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD);
        StringBuilder plain = new StringBuilder();
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            KeyEvent[] evs = c == '\n' ? null : map.getEvents(new char[] {c});
            if (c == '\n') {
                key(KeyEvent.KEYCODE_ENTER);
            } else if (evs != null) {
                for (KeyEvent e : evs) {
                    long now = SystemClock.uptimeMillis();
                    send(KeyEvent.changeTimeRepeat(e, now, 0));
                }
            } else {
                plain.setLength(0);
                while (i < text.length() && text.charAt(i) != '\n' && map.getEvents(new char[] {text.charAt(i)}) == null) {
                    plain.append(text.charAt(i++));
                }
                i--;
                paste(plain.toString());
            }
        }
    }

    private static void paste(String s) throws Exception {
        String before = getClip();
        setClip(s);
        key(KeyEvent.KEYCODE_PASTE);
        Thread.sleep(150);
        if (before != null) setClip(before);
    }
}
