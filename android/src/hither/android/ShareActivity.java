package hither.android;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.widget.ArrayAdapter;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.ProgressBar;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.util.ArrayList;
import java.util.List;

public final class ShareActivity extends Activity {
    static final String EXTRA_CLIP_FILE = "hither.clip_file";
    static final String EXTRA_CLIP_TEXT = "hither.clip_text";
    static final String EXTRA_PICK = "hither.pick";

    private final List<Peer> peers = new ArrayList<>();
    private final Handler main = new Handler(Looper.getMainLooper());
    private final Node.Listener listener = () -> main.post(this::refresh);
    private ArrayAdapter<Peer> adapter;
    private TextView status;
    private ProgressBar spin;
    private List<LocalSend.Item> items;
    private boolean pickOnly;
    private boolean sending;
    private int rounds;

    private final Runnable search = new Runnable() {
        @Override
        public void run() {
            Node.get(ShareActivity.this).announce(true);
            if (++rounds < 6) {
                main.postDelayed(this, 3000);
            } else {
                spin.setVisibility(View.GONE);
                if (peers.isEmpty()) status.setText("No devices found. Is hither or LocalSend open on the same Wi-Fi?");
            }
        }
    };

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
        Receiver.start(this);
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
        spin = new ProgressBar(this, null, android.R.attr.progressBarStyleSmall);
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
        refresh();
    }

    @Override
    protected void onStart() {
        super.onStart();
        if (adapter == null) return;
        Node node = Node.get(this);
        node.addListener(listener);
        node.picking(true);
        node.pool.execute(() -> {
            node.startQuietly();
            node.scan(60_000);
        });
        rounds = 0;
        main.post(search);
    }

    @Override
    protected void onStop() {
        main.removeCallbacks(search);
        Node node = Node.get(this);
        node.removeListener(listener);
        node.picking(false);
        super.onStop();
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

    private void refresh() {
        if (sending) return;
        Peer target = Hither.target(this);
        peers.clear();
        for (Peer p : Node.get(this).peers()) {
            if (!p.ip.isEmpty()) peers.add(p);
        }
        if (target != null) {
            for (int i = 0; i < peers.size(); i++) {
                if (peers.get(i).fingerprint.equalsIgnoreCase(target.fingerprint)) {
                    peers.add(0, peers.remove(i));
                    break;
                }
            }
        }
        adapter.notifyDataSetChanged();
        if (!peers.isEmpty()) status.setText("Tap a device");
    }

    private void pick(Peer p) {
        sending = true;
        main.removeCallbacks(search);
        Hither.trust(this, p.fingerprint);
        Hither.setTarget(this, p);
        Hither.remember(this, p);
        if (pickOnly) {
            finish();
            return;
        }
        status.setText("Sending to " + p.alias);
        Items.sendInBackground(this, p, items, this::finish);
    }
}
