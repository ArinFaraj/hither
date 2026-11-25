package hither.android;

import android.Manifest;
import android.app.Activity;
import android.app.StatusBarManager;
import android.content.ActivityNotFoundException;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.Switch;
import android.widget.TextView;
import android.widget.Toast;

public final class MainActivity extends Activity {
    private static final int SHOTS = 1;
    private static final int NOTIFY = 2;

    private TextView state;
    private TextView target;
    private Switch shots;
    private Button notify;
    private Button battery;
    private boolean updating;

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        Receiver.start(this);
        int pad = (int) (24 * getResources().getDisplayMetrics().density);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(pad, pad * 2, pad, pad);

        TextView title = new TextView(this);
        title.setTextAppearance(android.R.style.TextAppearance_DeviceDefault_Large);
        title.setText("Hither");
        root.addView(title);

        state = new TextView(this);
        state.setPadding(0, pad / 2, 0, pad / 2);
        root.addView(state);

        TextView about = new TextView(this);
        about.setPadding(0, 0, 0, pad);
        about.setText("Hither and LocalSend apps nearby can send to this phone. Files land in Downloads/Hither. "
                + "To send, share anything and pick Hither, select text and pick Hither, or use the Send clipboard tile.");
        root.addView(about);

        notify = button(root, "Allow notifications", v -> requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, NOTIFY));
        battery = button(root, "Keep receiving in the background", v -> open(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:" + getPackageName()))));

        target = new TextView(this);
        target.setPadding(0, pad / 2, 0, 0);
        root.addView(target);
        button(root, "Choose the device", v -> startActivity(new Intent(this, ShareActivity.class).putExtra(ShareActivity.EXTRA_PICK, true)));
        button(root, "Add the tile to Quick Settings", v -> getSystemService(StatusBarManager.class).requestAddTileService(
                new ComponentName(this, ClipTile.class), getString(R.string.tile_label),
                Icon.createWithResource(this, R.drawable.ic_tile), getMainExecutor(), r -> {}));

        shots = new Switch(this);
        shots.setPadding(0, pad, 0, pad);
        shots.setText("Send new screenshots to that device's clipboard");
        shots.setOnCheckedChangeListener((s, on) -> {
            if (updating) return;
            if (on && checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(new String[] {Manifest.permission.READ_MEDIA_IMAGES}, SHOTS);
                return;
            }
            Hither.setScreenshots(this, on);
        });
        root.addView(shots);

        button(root, "Quick Share visibility", v -> open(new Intent("com.google.android.gms.settings.SHARING")));

        ScrollView scroll = new ScrollView(this);
        scroll.setFitsSystemWindows(true);
        scroll.addView(root);
        setContentView(scroll);
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED && b == null) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, NOTIFY);
        }
    }

    private Button button(LinearLayout root, String label, View.OnClickListener l) {
        Button btn = new Button(this);
        btn.setText(label);
        btn.setOnClickListener(l);
        root.addView(btn);
        return btn;
    }

    private void open(Intent i) {
        try {
            startActivity(i);
        } catch (ActivityNotFoundException | SecurityException e) {
            Toast.makeText(this, "This phone does not have that setting", Toast.LENGTH_SHORT).show();
        }
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        boolean ok = results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED;
        if (code == SHOTS) {
            Hither.setScreenshots(this, ok);
            shots.setChecked(ok);
        }
        update();
    }

    @Override
    protected void onResume() {
        super.onResume();
        update();
    }

    private void update() {
        Node node = Node.get(this);
        String ip = Node.lanAddress();
        if (!node.running()) state.setText("Starting...");
        else if (ip == null) state.setText("Waiting for Wi-Fi. Shows up as \"" + Hither.name(this) + "\" once connected.");
        else state.setText("Ready to receive as \"" + Hither.name(this) + "\" at " + ip + ":" + node.port());
        if (!node.running()) state.postDelayed(this::update, 1000);
        Peer p = Hither.target(this);
        target.setText(p == null ? "Tile, text selection and screenshots go to: not chosen yet" : "Tile, text selection and screenshots go to " + p.alias);
        updating = true;
        shots.setChecked(Hither.screenshots(this) && checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) == PackageManager.PERMISSION_GRANTED);
        updating = false;
        notify.setVisibility(checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED ? View.GONE : View.VISIBLE);
        boolean free = getSystemService(PowerManager.class).isIgnoringBatteryOptimizations(getPackageName());
        battery.setVisibility(free ? View.GONE : View.VISIBLE);
    }
}
