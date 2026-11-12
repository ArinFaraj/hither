package hither.android;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;

import java.net.Socket;
import java.security.GeneralSecurityException;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.security.Principal;
import java.security.PrivateKey;
import java.security.cert.X509Certificate;
import java.security.spec.ECGenParameterSpec;

import javax.net.ssl.KeyManager;
import javax.net.ssl.SSLEngine;
import javax.net.ssl.X509ExtendedKeyManager;
import javax.security.auth.x500.X500Principal;

final class Identity {
    private static final String ALIAS = "hither-localsend";
    private static Identity cached;

    final X509Certificate cert;
    final PrivateKey key;
    final String fingerprint;

    private Identity(X509Certificate cert, PrivateKey key) throws GeneralSecurityException {
        this.cert = cert;
        this.key = key;
        byte[] d = MessageDigest.getInstance("SHA-256").digest(cert.getEncoded());
        StringBuilder sb = new StringBuilder();
        for (byte b : d) sb.append(String.format("%02X", b));
        fingerprint = sb.toString();
    }

    static synchronized Identity get() throws GeneralSecurityException {
        if (cached != null) return cached;
        try {
            KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
            ks.load(null);
            if (!ks.containsAlias(ALIAS)) {
                KeyPairGenerator g = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore");
                g.initialize(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
                        .setAlgorithmParameterSpec(new ECGenParameterSpec("secp256r1"))
                        .setDigests(KeyProperties.DIGEST_NONE, KeyProperties.DIGEST_SHA256, KeyProperties.DIGEST_SHA384, KeyProperties.DIGEST_SHA512)
                        .setCertificateSubject(new X500Principal("CN=hither"))
                        .build());
                g.generateKeyPair();
            }
            cached = new Identity((X509Certificate) ks.getCertificate(ALIAS), (PrivateKey) ks.getKey(ALIAS, null));
            return cached;
        } catch (java.io.IOException e) {
            throw new GeneralSecurityException(e);
        }
    }

    KeyManager[] keyManagers() {
        return new KeyManager[] {new X509ExtendedKeyManager() {
            @Override
            public String chooseClientAlias(String[] types, Principal[] issuers, Socket s) {
                return ALIAS;
            }

            @Override
            public String chooseEngineClientAlias(String[] types, Principal[] issuers, SSLEngine e) {
                return ALIAS;
            }

            @Override
            public X509Certificate[] getCertificateChain(String alias) {
                return ALIAS.equals(alias) ? new X509Certificate[] {cert} : null;
            }

            @Override
            public PrivateKey getPrivateKey(String alias) {
                return ALIAS.equals(alias) ? key : null;
            }

            @Override
            public String[] getClientAliases(String type, Principal[] issuers) {
                return new String[] {ALIAS};
            }

            @Override
            public String chooseServerAlias(String type, Principal[] issuers, Socket s) {
                return null;
            }

            @Override
            public String[] getServerAliases(String type, Principal[] issuers) {
                return null;
            }
        }};
    }
}
