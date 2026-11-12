package drop.android;

import android.Manifest;
import android.app.Activity;
import android.app.StatusBarManager;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.Switch;
import android.widget.TextView;

public final class MainActivity extends Activity {
    private TextView target;
    private Switch shots;

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        int pad = (int) (24 * getResources().getDisplayMetrics().density);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(pad, pad * 2, pad, pad);
        root.setFitsSystemWindows(true);

        TextView title = new TextView(this);
        title.setTextAppearance(android.R.style.TextAppearance_DeviceDefault_Large);
        title.setText("drop");
        root.addView(title);

        TextView about = new TextView(this);
        about.setPadding(0, pad / 2, 0, pad);
        about.setText("This phone shows up as \"" + Drop.name(this) + "\". Share anything and pick drop, "
                + "or use the Send clipboard tile in Quick Settings.");
        root.addView(about);

        target = new TextView(this);
        root.addView(target);

        Button change = new Button(this);
        change.setText("Choose tile device");
        change.setOnClickListener(v -> startActivity(new Intent(this, ShareActivity.class).putExtra(ShareActivity.EXTRA_PICK, true)));
        root.addView(change);

        Button tile = new Button(this);
        tile.setText("Add the tile to Quick Settings");
        tile.setOnClickListener(v -> getSystemService(StatusBarManager.class).requestAddTileService(
                new ComponentName(this, ClipTile.class), getString(R.string.tile_label),
                Icon.createWithResource(this, R.drawable.ic_tile), getMainExecutor(), r -> {}));
        root.addView(tile);

        shots = new Switch(this);
        shots.setPadding(0, pad, 0, 0);
        shots.setText("Copy new screenshots to the clipboard");
        shots.setOnCheckedChangeListener((s, on) -> {
            if (on && checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(new String[] {Manifest.permission.READ_MEDIA_IMAGES}, 1);
                return;
            }
            Drop.setScreenshots(this, on);
        });
        root.addView(shots);

        setContentView(root);
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        boolean ok = results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED;
        Drop.setScreenshots(this, ok);
        shots.setChecked(ok);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Peer p = Drop.target(this);
        target.setText(p == null ? "Tile sends to: not chosen yet" : "Tile sends to: " + p.alias);
        shots.setChecked(Drop.screenshots(this));
    }
}
