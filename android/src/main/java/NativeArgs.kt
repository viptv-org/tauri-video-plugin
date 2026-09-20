package io.github.taurivideo.plugin

import android.net.Uri
import androidx.media3.exoplayer.DefaultLoadControl
import app.tauri.annotation.InvokeArg

internal data class NativeBufferDurations(
    val minMs: Int,
    val maxMs: Int,
    val playbackMs: Int,
    val rebufferMs: Int,
)

internal fun resolveRequestedBufferDurations(args: NativeOpenArgs): NativeBufferDurations? {
    if (
        args.minBufferMs == null && args.maxBufferMs == null &&
        args.playbackBufferMs == null && args.rebufferMs == null
    ) return null

    val requestedMaxMs = args.maxBufferMs?.coerceIn(1_000, 180_000)
    val minMs = (args.minBufferMs
        ?: requestedMaxMs?.coerceAtMost(DefaultLoadControl.DEFAULT_MIN_BUFFER_MS)
        ?: DefaultLoadControl.DEFAULT_MIN_BUFFER_MS)
        .coerceIn(1_000, 120_000)
    val maxMs = (requestedMaxMs ?: DefaultLoadControl.DEFAULT_MAX_BUFFER_MS)
        .coerceIn(minMs, 180_000)
    return NativeBufferDurations(
        minMs = minMs,
        maxMs = maxMs,
        playbackMs = (args.playbackBufferMs ?: DefaultLoadControl.DEFAULT_BUFFER_FOR_PLAYBACK_MS)
            .coerceIn(250, minMs),
        rebufferMs = (
            args.rebufferMs
                ?: DefaultLoadControl.DEFAULT_BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS
            ).coerceIn(500, minMs),
    )
}

internal fun resolveRequestedTargetBufferBytes(requestedBytes: Long?): Int? = requestedBytes
    ?.coerceIn(8L * 1024 * 1024, 512L * 1024 * 1024)
    ?.toInt()

internal fun containerFromUri(value: String): String {
    val name = Uri.parse(value).lastPathSegment?.substringBefore('?')?.lowercase() ?: return "unknown"
    return when {
        name.endsWith(".mkv") || name.endsWith(".mka") -> "matroska"
        name.endsWith(".webm") -> "webm"
        name.endsWith(".mp4") || name.endsWith(".m4v") -> "mp4"
        name.endsWith(".avi") -> "avi"
        name.endsWith(".ts") || name.endsWith(".m2ts") -> "mpeg-ts"
        name.endsWith(".mov") -> "quicktime"
        else -> "unknown"
    }
}

@InvokeArg class NativeOpenArgs {
    var sessionKey: String = ""
    var uri: String = ""; var x: Double = 0.0; var y: Double = 0.0
    var scrollX: Double = 0.0; var scrollY: Double = 0.0
    var backend: String? = null
    var width: Double = 1.0; var height: Double = 1.0; var autoplay: Boolean = false
    var volume: Double = 1.0; var muted: Boolean = false
    var headers: HashMap<String, String> = HashMap()
    var cookies: String? = null; var userAgent: String? = null; var referrer: String? = null
    var tlsCaFile: String? = null; var startPositionSeconds: Double? = null
    var minBufferMs: Int? = null; var maxBufferMs: Int? = null
    var playbackBufferMs: Int? = null; var rebufferMs: Int? = null
    var targetBufferBytes: Long? = null; var decoderFallback: Boolean? = null
    var dolbyVisionMode: String? = null; var tunneling: Boolean? = null
}
@InvokeArg class NativeLayoutArgs {
    var sessionKey: String = ""
    var x: Double = 0.0; var y: Double = 0.0; var width: Double = 1.0; var height: Double = 1.0
    var scrollX: Double = 0.0; var scrollY: Double = 0.0
}
@InvokeArg class NativeControlArgs {
    var sessionKey: String = ""
    var action: String = ""; var value: Double = 0.0; var index: Int = -1
}
@InvokeArg class NativeSessionArgs { var sessionKey: String = "" }
