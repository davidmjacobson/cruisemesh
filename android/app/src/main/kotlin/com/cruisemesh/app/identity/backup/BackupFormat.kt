package com.cruisemesh.app.identity.backup

/** Platform-facing backup data; its serialization and encryption live in Rust. */
data class BackupPayload(
    val identity: ByteArray,
    val sqlite: ByteArray,
    val srcVersionCode: Int,
    val createdAtMs: Long,
    val displayName: String? = null,
    val ownAvatar: ByteArray = ByteArray(0),
    val ownAvatarEpoch: Long = 0L,
    val relayUrl: String? = null,
    val relayToken: String? = null,
    val shareOnline: Boolean = true,
) {
    override fun equals(other: Any?): Boolean =
        other is BackupPayload &&
            identity.contentEquals(other.identity) &&
            sqlite.contentEquals(other.sqlite) &&
            srcVersionCode == other.srcVersionCode &&
            createdAtMs == other.createdAtMs &&
            displayName == other.displayName &&
            ownAvatar.contentEquals(other.ownAvatar) &&
            ownAvatarEpoch == other.ownAvatarEpoch &&
            relayUrl == other.relayUrl &&
            relayToken == other.relayToken &&
            shareOnline == other.shareOnline

    override fun hashCode(): Int {
        var result = identity.contentHashCode()
        result = 31 * result + sqlite.contentHashCode()
        result = 31 * result + srcVersionCode
        result = 31 * result + createdAtMs.hashCode()
        result = 31 * result + (displayName?.hashCode() ?: 0)
        result = 31 * result + ownAvatar.contentHashCode()
        result = 31 * result + ownAvatarEpoch.hashCode()
        result = 31 * result + (relayUrl?.hashCode() ?: 0)
        result = 31 * result + (relayToken?.hashCode() ?: 0)
        result = 31 * result + shareOnline.hashCode()
        return result
    }
}

sealed class BackupException(message: String) : Exception(message) {
    data class NewerBackup(val srcVersionCode: Int, val appVersionCode: Int) :
        BackupException(
            "This backup is from a newer app version ($srcVersionCode > $appVersionCode); update CruiseMesh first",
        )
}
