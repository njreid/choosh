package ai.choosh;

import java.util.Objects;

/**
 * Android outer-composition factory for the bounded callback capability used by one SSH attempt.
 *
 * <p>This is deliberately a small, constructor-injected assembly point rather than a registry.
 * It joins the platform socket-opening capability with the Android-owned public-key and
 * payload-only signing capabilities, then returns a fresh {@link BoundedAndroidNativeRuntime}
 * for the selected connection. The returned runtime owns no durable profile state and exposes no
 * credential material. Its lease, rather than this factory, owns the opened socket.</p>
 *
 * <p>The eventual Activity composition root supplies a durable profile source and one instance of
 * this factory to {@link AndroidGitStatusComposition}; neither presentation code nor JNI needs
 * direct access to sockets or Keystore implementations.</p>
 */
public final class AndroidRuntimeComposition {
    private final RustNativeConnectorJni.NativeHandleResolver handles;
    private final BoundedAndroidSocketAdapter.SocketOpener socketOpener;
    private final BoundedAndroidSocketAdapter.Limits socketLimits;
    private final BoundedAndroidNativeRuntime.PublicKeySource publicKeys;
    private final BoundedAndroidNativeRuntime.LeaseSignerSource signers;

    public AndroidRuntimeComposition(
        RustNativeConnectorJni.NativeHandleResolver handles,
        BoundedAndroidSocketAdapter.SocketOpener socketOpener,
        BoundedAndroidSocketAdapter.Limits socketLimits,
        BoundedAndroidNativeRuntime.PublicKeySource publicKeys,
        BoundedAndroidNativeRuntime.LeaseSignerSource signers
    ) {
        this.handles = Objects.requireNonNull(handles, "handles");
        this.socketOpener = Objects.requireNonNull(socketOpener, "socketOpener");
        this.socketLimits = Objects.requireNonNull(socketLimits, "socketLimits");
        this.publicKeys = Objects.requireNonNull(publicKeys, "publicKeys");
        this.signers = Objects.requireNonNull(signers, "signers");
    }

    /**
     * Creates a new, unshared runtime for one connection attempt.
     *
     * <p>No network activity occurs here. Socket opening happens only when the planned native
     * connector acquires the resulting runtime for its typed input.</p>
     */
    public BoundedAndroidNativeRuntime newRuntime() {
        return new BoundedAndroidNativeRuntime(
            handles,
            new BoundedAndroidSocketAdapter(socketOpener, socketLimits),
            publicKeys,
            signers
        );
    }

    /**
     * Convenience outer-root constructor for the platform's normal {@link java.net.Socket}
     * implementation. Tests can instead inject a deterministic {@link
     * BoundedAndroidSocketAdapter.SocketOpener} through the main constructor.
     */
    public static AndroidRuntimeComposition withJvmSockets(
        RustNativeConnectorJni.NativeHandleResolver handles,
        BoundedAndroidSocketAdapter.SocketFactory sockets,
        BoundedAndroidSocketAdapter.Limits socketLimits,
        BoundedAndroidNativeRuntime.PublicKeySource publicKeys,
        BoundedAndroidNativeRuntime.LeaseSignerSource signers
    ) {
        return new AndroidRuntimeComposition(
            handles,
            new BoundedAndroidSocketAdapter.JvmSocketOpener(
                Objects.requireNonNull(sockets, "sockets")
            ),
            socketLimits,
            publicKeys,
            signers
        );
    }
}
