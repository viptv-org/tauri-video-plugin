package io.github.taurivideo.plugin

import android.os.SystemClock
import androidx.media3.common.C
import androidx.media3.common.VideoSize
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.upstream.DefaultAllocator
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject

/**
 * Mutable snapshot state owned by the single [VideoPlugin] instance. The
 * media3 frame-rate sampler and the track caches have to stay
 * single-instance so consecutive snapshots measure deltas against the same
 * baseline, which is why this state is passed explicitly to the snapshot
 * codecs instead of living in module globals.
 */
internal class NativeSnapshotState {
    var videoDecoderName = "uninitialized"
    var lastRenderedFrames = 0L
    var lastFrameSampleNs = 0L
    var measuredFps = 0.0
    var videoSize = VideoSize.UNKNOWN
    var allocator: DefaultAllocator? = null
    var nativeContainer = "unknown"
    var cachedNativeTracks: JSArray = JSArray()
    var cachedVlcTrackSource: List<VlcTrack>? = null
    var cachedVlcTracks: JSArray = JSArray()
}

internal fun nativeSnapshot(player: ExoPlayer, state: NativeSnapshotState): JSObject {
    val counters = player.videoDecoderCounters
    counters?.ensureUpdated()
    val presentedFrames = counters?.renderedOutputBufferCount?.toLong()?.coerceAtLeast(0) ?: 0L
    val droppedFrames = counters?.droppedBufferCount?.toLong()?.coerceAtLeast(0) ?: 0L
    val nowNs = SystemClock.elapsedRealtimeNanos()
    if (state.lastFrameSampleNs == 0L) {
        state.lastFrameSampleNs = nowNs
        state.lastRenderedFrames = presentedFrames
    } else if (nowNs - state.lastFrameSampleNs >= 500_000_000L) {
        val elapsedSeconds = (nowNs - state.lastFrameSampleNs) / 1_000_000_000.0
        state.measuredFps = (presentedFrames - state.lastRenderedFrames).coerceAtLeast(0) / elapsedSeconds
        state.lastFrameSampleNs = nowNs
        state.lastRenderedFrames = presentedFrames
    }
    val processingCount = counters?.videoFrameProcessingOffsetCount ?: 0
    val averageProcessingUs = if (processingCount > 0) {
        (counters?.totalVideoFrameProcessingOffsetUs ?: 0L).toDouble() / processingCount
    } else 0.0
    val durationMs = player.duration.takeIf { it != C.TIME_UNSET }?.coerceAtLeast(0) ?: 0
    val live = player.isCurrentMediaItemLive
    val seekable = player.isCurrentMediaItemSeekable
    val seekableEndMs = if (durationMs > 0) durationMs else maxOf(
        player.currentPosition,
        player.bufferedPosition,
    ).coerceAtLeast(0)
    return JSObject().apply {
        put("durationSeconds", durationMs / 1000.0)
        put("currentTimeSeconds", player.currentPosition.coerceAtLeast(0) / 1000.0)
        put("bufferedSeconds", player.bufferedPosition.coerceAtLeast(0) / 1000.0)
        put("live", live)
        put("seekable", seekable)
        put("seekableStartSeconds", 0.0)
        put("seekableEndSeconds", seekableEndMs / 1000.0)
        put("playing", player.isPlaying)
        put("videoWidth", state.videoSize.width)
        put("videoHeight", state.videoSize.height)
        put("presentedFrames", presentedFrames)
        put("droppedFrames", droppedFrames)
        put("measuredFps", state.measuredFps)
        put("hardwareBackend", "android-mediacodec:${state.videoDecoderName}:surface-view")
        put("encodedBytesBuffered", state.allocator?.totalBytesAllocated?.toLong() ?: 0L)
        put("averageFrameProcessingUs", averageProcessingUs)
        put("container", state.nativeContainer)
        put("tracks", state.cachedNativeTracks)
    }
}

internal fun vlcSnapshot(snapshot: VlcSnapshot, state: NativeSnapshotState): JSObject {
    val tracks = vlcTrackArray(snapshot.tracks, state)
    return JSObject().apply {
        put("durationSeconds", snapshot.durationSeconds)
        put("currentTimeSeconds", snapshot.currentTimeSeconds)
        put("bufferedSeconds", snapshot.bufferedSeconds)
        put("live", snapshot.live)
        put("seekable", snapshot.seekable)
        put("seekableStartSeconds", snapshot.seekableStartSeconds)
        put("seekableEndSeconds", snapshot.seekableEndSeconds)
        put("playing", snapshot.playing)
        put("videoWidth", snapshot.videoWidth)
        put("videoHeight", snapshot.videoHeight)
        put("presentedFrames", snapshot.presentedFrames)
        put("droppedFrames", snapshot.droppedFrames)
        put("measuredFps", snapshot.measuredFps)
        put("hardwareBackend", snapshot.hardwareBackend)
        put("encodedBytesBuffered", snapshot.encodedBytesBuffered)
        put("averageFrameProcessingUs", snapshot.averageFrameProcessingUs)
        put("container", snapshot.container)
        put("tracks", tracks)
    }
}

internal fun vlcTrackArray(tracks: List<VlcTrack>, state: NativeSnapshotState): JSArray {
    if (state.cachedVlcTrackSource === tracks) return state.cachedVlcTracks
    state.cachedVlcTrackSource = tracks
    state.cachedVlcTracks = JSArray(tracks.map { track ->
        JSObject().apply {
            put("id", track.id.toString())
            put("index", track.id)
            put("kind", track.kind)
            put("language", track.language)
            put("label", track.label)
            put("codec", track.codec)
            put("selected", track.selected)
        }
    })
    return state.cachedVlcTracks
}
