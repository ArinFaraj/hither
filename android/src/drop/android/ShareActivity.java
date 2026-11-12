package drop.android;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.view.View;
import android.widget.ArrayAdapter;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.ProgressBar;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

public final class ShareActivity extends Activity {
    static final String EXTRA_CLIP_FILE = "drop.clip_file";
    static final String EXTRA_CLIP_TEXT = "drop.clip_text";
    static final String EXTRA_PICK = "drop.pick";

    private final List<Peer> peers = new ArrayList<>();
    private ArrayAdapter<Peer> adapter;
    private TextView status;
    private volatile boolean searching = true;
    private List<LocalSend.Item> items;
    private boolean pickOnly;

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        pickOnly = getIntent().getBooleanExtra(EXTRA_PICK, false);
        items = pickOnly ? new ArrayList<>() : load(getIntent());
        if (items.isEmpty() && !pickOnly) {
            Toast.makeText(this, "Nothing to send", Toast.LENGTH_SHORT).show();
            finish();
            return;
        }
        int pad = (int) (20 * getResources().getDisplayMetrics().density);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(pad, pad, pad, pad / 2);
        TextView title = new TextView(this);
        title.setTextAppearance(android.R.style.TextAppearance_DeviceDefault_DialogWindowTitle);
        title.setText(pickOnly ? "Tile sends to" : describe(items));
        root.addView(title);
        LinearLayout row = new LinearLayout(this);
        row.setPadding(0, pad / 2, 0, pad / 2);
        ProgressBar spin = new ProgressBar(this, null, android.R.attr.progressBarStyleSmall);
        row.addView(spin);
        status = new TextView(this);
        status.setPadding(pad / 2, 0, 0, 0);
        status.setText("Looking for devices on this network");
        row.addView(status);
        root.addView(row);
        ListView list = new ListView(this);
        adapter = new ArrayAdapter<>(this, android.R.layout.simple_list_item_1, peers);
        list.setAdapter(adapter);
        list.setOnItemClickListener((parent, view, pos, id) -> pick(peers.get(pos)));
        root.addView(list, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, (int) (280 * getResources().getDisplayMetrics().density)));
        setContentView(root);
        setFinishOnTouchOutside(true);
        Peer saved = Drop.target(this);
        if (saved != null) add(saved);
        new Thread(() -> {
            try {
                while (searching) LocalSend.discover(this, 8000, p -> runOnUiThread(() -> add(p)));
            } catch (IOException e) {
                runOnUiThread(() -> status.setText("Discovery failed: " + e.getMessage()));
            }
            runOnUiThread(() -> spin.setVisibility(View.GONE));
        }).start();
    }

    private List<LocalSend.Item> load(Intent i) {
        String clipFile = i.getStringExtra(EXTRA_CLIP_FILE);
        if (clipFile != null) {
            List<LocalSend.Item> l = new ArrayList<>();
            l.add(Items.file(new File(clipFile), Items.CLIP_IMAGE, "image/png"));
            return l;
        }
        String clipText = i.getStringExtra(EXTRA_CLIP_TEXT);
        if (clipText != null) {
            List<LocalSend.Item> l = new ArrayList<>();
            l.add(Items.text(clipText, Items.CLIP_TEXT));
            return l;
        }
        return Items.fromShare(this, i);
    }

    private static String describe(List<LocalSend.Item> items) {
        if (items.size() == 1) {
            LocalSend.Item it = items.get(0);
            if (it.text != null) return Items.CLIP_TEXT.equals(it.name) ? "Send clipboard" : "Send text";
            return Items.CLIP_IMAGE.equals(it.name) ? "Send clipboard image" : "Send " + it.name;
        }
        return "Send " + items.size() + " files";
    }

    private void add(Peer p) {
        for (int i = 0; i < peers.size(); i++) {
            Peer q = peers.get(i);
            if (q.fingerprint.equalsIgnoreCase(p.fingerprint) || q.alias.equalsIgnoreCase(p.alias)) {
                peers.set(i, p);
                adapter.notifyDataSetChanged();
                return;
            }
        }
        peers.add(p);
        adapter.notifyDataSetChanged();
        status.setText("Tap a device");
    }

    private void pick(Peer p) {
        searching = false;
        Drop.setTarget(this, p);
        if (pickOnly) {
            finish();
            return;
        }
        status.setText("Sending to " + p.alias);
        Items.sendInBackground(this, p, items, this::finish);
    }

    @Override
    protected void onDestroy() {
        searching = false;
        super.onDestroy();
    }
}
