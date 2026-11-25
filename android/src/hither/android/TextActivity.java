package hither.android;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.Toast;

import java.util.Collections;

public final class TextActivity extends Activity {
    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        CharSequence t = getIntent().getCharSequenceExtra(Intent.EXTRA_PROCESS_TEXT);
        if (t == null || t.length() == 0) {
            Toast.makeText(this, "Nothing selected", Toast.LENGTH_SHORT).show();
        } else {
            Peer target = Hither.target(this);
            if (target == null) {
                startActivity(new Intent(this, ShareActivity.class).putExtra(ShareActivity.EXTRA_CLIP_TEXT, t.toString()));
            } else {
                Items.sendInBackground(this, target, Collections.singletonList(Items.text(t.toString(), Items.CLIP_TEXT)), null);
            }
        }
        finish();
    }
}
