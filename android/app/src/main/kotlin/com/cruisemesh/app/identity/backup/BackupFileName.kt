package com.cruisemesh.app.identity.backup

/**
 * Decides what to call the backup document a person picked.
 *
 * A Storage Access Framework uri is not a path. Its last segment is a provider
 * document id, and what that id looks like is entirely the provider's business:
 * the Downloads provider hands back a bare row number (`152`), the external
 * storage provider hands back `primary:Download/whatever.cmbak`, and a cloud
 * provider can hand back an opaque token. Showing that raw is how the restore
 * screen ended up captioned with a number, which tells the person nothing about
 * which of their backups they just opened.
 *
 * The provider's display-name column is the real answer. This object exists for
 * the case where no provider answers it: rather than fall back to the id, it
 * salvages the segment **only** when the segment is itself a filename, and
 * otherwise returns null so the caller can show generic copy from resources.
 *
 * Pure — no Android imports — so the rule is unit-testable on the JVM.
 */
internal object BackupFileName {

    /** Longest run of characters after the final `.` still read as an extension. */
    private const val MAX_EXTENSION_LENGTH = 8

    /**
     * The label for a picked document, or null when nothing trustworthy is
     * available and the caller should use generic copy.
     *
     * @param providerDisplayName `OpenableColumns.DISPLAY_NAME` as the content
     *   resolver reported it, or null when the query returned no row.
     * @param lastPathSegment the uri's last path segment, i.e. the document id.
     */
    fun resolve(providerDisplayName: String?, lastPathSegment: String?): String? =
        providerDisplayName?.trim()?.takeIf { it.isNotEmpty() }
            ?: salvageFromDocumentId(lastPathSegment)

    /**
     * A document id is worth showing only when it ends in something a person
     * would recognise as a file name. Ids are commonly `primary:Download/a.cmbak`
     * or `raw:/storage/emulated/0/a.cmbak`, both of which carry the real name
     * after the last separator; ids that are bare numbers or opaque tokens carry
     * nothing and are rejected.
     */
    private fun salvageFromDocumentId(lastPathSegment: String?): String? {
        val candidate = lastPathSegment
            ?.substringAfterLast('/')
            ?.substringAfterLast(':')
            ?.trim()
            ?: return null
        return candidate.takeIf { looksLikeFileName(it) }
    }

    /**
     * True when the candidate has a non-empty stem and an extension that reads
     * like one: short, alphanumeric, and containing at least one letter. The
     * letter is what keeps a version-numbered id such as `1.2` from passing as a
     * file name.
     */
    private fun looksLikeFileName(candidate: String): Boolean {
        val dot = candidate.lastIndexOf('.')
        if (dot <= 0 || dot == candidate.length - 1) return false
        val extension = candidate.substring(dot + 1)
        if (extension.length > MAX_EXTENSION_LENGTH) return false
        if (!extension.all { it.isLetterOrDigit() }) return false
        return extension.any { it.isLetter() }
    }
}
