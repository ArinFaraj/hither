package drop.android;

import android.app.Activity;
import android.content.ClipboardManager;
import android.content.Intent;
import android.widget.Toast;

import java.io.IOException;
import java.util.List;

public final class ClipActivity extends Activity {
    private boolean done;

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (!focused || done) return;
        done = true;
        List<LocalSend.Item> items;
        try {
            items = Items.fromClipboard(this, getSystemService(ClipboardManager.class).getPrimaryClip());
        } catch (IOException | RuntimeException e) {
            Toast.makeText(this, "Could not read the clipboard: " + e.getMessage(), Toast.LENGTH_SHORT).show();
            finish();
            return;
        }
        if (items.isEmpty()) {
            Toast.makeText(this, "The clipboard is empty", Toast.LENGTH_SHORT).show();
            finish();
            return;
        }
        Peer target = Drop.target(this);
        if (target == null) {
            LocalSend.Item it = items.get(0);
            Intent i = new Intent(this, ShareActivity.class);
            if (it.text != null) i.putExtra(ShareActivity.EXTRA_CLIP_TEXT, it.text);
            else i.putExtra(ShareActivity.EXTRA_CLIP_FILE, it.file.getPath());
            startActivity(i);
            finish();
            return;
        }
        Items.sendInBackground(this, target, items, null);
        finish();
    }
}
