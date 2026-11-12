package drop.android;

import android.app.PendingIntent;
import android.content.Intent;
import android.service.quicksettings.Tile;
import android.service.quicksettings.TileService;

public final class ClipTile extends TileService {
    @Override
    public void onStartListening() {
        Tile t = getQsTile();
        if (t == null) return;
        Peer p = Drop.target(this);
        t.setState(Tile.STATE_INACTIVE);
        t.setSubtitle(p == null ? "Pick a device" : "to " + p.alias);
        t.updateTile();
    }

    @Override
    public void onClick() {
        Intent i = new Intent(this, ClipActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        startActivityAndCollapse(PendingIntent.getActivity(this, 0, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
    }
}
