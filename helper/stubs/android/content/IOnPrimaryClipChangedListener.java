package android.content;

public interface IOnPrimaryClipChangedListener extends android.os.IInterface {
    void dispatchPrimaryClipChanged() throws android.os.RemoteException;

    abstract class Stub extends android.os.Binder implements IOnPrimaryClipChangedListener {
        public Stub() {
            throw new RuntimeException("stub");
        }

        public android.os.IBinder asBinder() {
            return this;
        }
    }
}
