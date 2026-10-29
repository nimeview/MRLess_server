package local.mrless.app

object NativeNode {
    init {
        System.loadLibrary("mrless")
    }

    external fun start(dataDir: String, name: String?): Int
    external fun stop()
    external fun wsPort(): Int
    external fun meshPort(): Int
    external fun reconnectPeers(): Int
}
