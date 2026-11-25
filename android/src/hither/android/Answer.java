package hither.android;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public final class Answer extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent i) {
        String id = i.getStringExtra("id");
        Inbox.answer(id, i.getStringExtra("choice"));
        if (id != null) Notes.cancel(c, id.hashCode());
    }
}
