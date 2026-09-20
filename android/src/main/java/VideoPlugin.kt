package io.github.taurivideo.plugin

import android.app.Activity
import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.view.SurfaceView
import android.view.WindowManager
import android.webkit.WebSettings
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.VideoSize
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.mediacodec.MediaCodecSelector
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.exoplayer.upstream.DefaultAllocator
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import org.videolan.libvlc.util.VLCVideoLayout

/** Android/TV integration with a direct SurfaceView playback plane. */
@TauriPlugin
class VideoPlugin(private val activity: Activity) : Plugin(activity) {
    private val audioManager = activity.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private var focusRequest: AudioFocusRequest? = null
    private var nativeRoot: FrameLayout? = null
    private var nativeView: PlayerView? = null
    private var vlcView: VLCVideoLayout? = null
    private var hostWebView: WebView? = null
    private val nativeSurfaceSync = NativeSurfaceSync({ nativeRoot }, { hostWebView })
    private val snapshotState = NativeSnapshotState()
    private var nativePlayer: ExoPlayer? = null
    private var vlcPlayer: VlcPlayer? = null
    private var openGeneration = 0
    private var activeSessionKey: String? = null
    private val trackTargets = HashMap<Int, Pair<Tracks.Group, Int>>()
    private val bundledCaFile: File?

    init {
        bundledCaFile = runCatching {
            val trustDirectory = File(activity.filesDir, "tauri-video-ca").apply { mkdirs() }
            File(trustDirectory, "ca-certificates.crt").also { destination ->
                activity.assets.open("tauri-video-ca-certificates.crt").use { input ->
                    destination.outputStream().use(input::copyTo)
                }
            }
        }.getOrNull()
    }

    override fun load(webView: WebView) {
        webView.settings.mediaPlaybackRequiresUserGesture = false
        webView.settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        var transparentAncestor: View? = webView
        while (transparentAncestor != null && transparentAncestor !== activity.window.decorView) {
            transparentAncestor.setBackgroundColor(Color.TRANSPARENT)
            transparentAncestor = transparentAncestor.parent as? View
        }
        activity.runOnUiThread {
            nativeSurfaceSync.unregister()
            hostWebView = webView
            nativeSurfaceSync.registerIfNeeded()
            ensureNativeSurfaceHost()
        }
    }

    /** Create the native playback plane beneath Tauri's Android WebView. */
    private fun ensureNativeSurfaceHost() {
        if (nativeRoot != null && nativeView != null && vlcView != null) return
        val nativeContainer = FrameLayout(activity).apply {
            setBackgroundColor(Color.BLACK)
            visibility = View.GONE
        }
        val playerView = PlayerView(activity).apply {
            useController = false
            resizeMode = AspectRatioFrameLayout.RESIZE_MODE_FIT
            setShutterBackgroundColor(Color.BLACK)
            val uiType = resources.configuration.uiMode and Configuration.UI_MODE_TYPE_MASK
            subtitleView?.setBottomPaddingFraction(
                if (uiType == Configuration.UI_MODE_TYPE_TELEVISION) 0.24f else 0.16f
            )
        }
        val vlcLayout = VLCVideoLayout(activity).apply { visibility = View.GONE }
        val match = FrameLayout.LayoutParams(
            FrameLayout.LayoutParams.MATCH_PARENT,
            FrameLayout.LayoutParams.MATCH_PARENT,
        )
        nativeContainer.addView(playerView, match)
        nativeContainer.addView(vlcLayout, match)
        val root = activity.findViewById<ViewGroup>(android.R.id.content)
        root.addView(nativeContainer, 0, FrameLayout.LayoutParams(1, 1))
        rendererSurfaces(nativeContainer).forEach {
            it.setZOrderOnTop(false)
            it.setZOrderMediaOverlay(false)
        }
        nativeRoot = nativeContainer
        nativeView = playerView
        vlcView = vlcLayout
    }

    private fun rendererSurfaces(root: View): List<SurfaceView> {
        if (root is SurfaceView) return listOf(root)
        if (root !is ViewGroup) return emptyList()
        return buildList {
            for (index in 0 until root.childCount) {
                addAll(rendererSurfaces(root.getChildAt(index)))
            }
        }
    }

    @Command
    fun openNative(invoke: Invoke) {
        val args = invoke.parseArgs(NativeOpenArgs::class.java)
        activity.runOnUiThread {
            closeNativePlayer()
            if (hostWebView == null) {
                invoke.reject("native video requires an initialized Tauri WebView")
                return@runOnUiThread
            }
            ensureNativeSurfaceHost()
            activeSessionKey = args.sessionKey
            val generation = openGeneration
            val view = nativeView
            val root = nativeRoot
            if (view == null || root == null) {
                invoke.reject("native video surface is unavailable")
                return@runOnUiThread
            }
            nativeSurfaceSync.applyLayout(
                args.x,
                args.y,
                args.width,
                args.height,
                args.scrollX,
                args.scrollY,
            )
            root.visibility = View.VISIBLE
            view.visibility = View.VISIBLE
            vlcView?.visibility = View.GONE
            view.videoSurfaceView?.apply {
                scaleX = 1f
                scaleY = 1f
            }
            snapshotState.nativeContainer = containerFromUri(args.uri)
            val requestedBackend = args.backend?.lowercase() ?: "media3"
            if (requestedBackend !in setOf("media3", "libvlc")) {
                root.visibility = View.GONE
                nativeSurfaceSync.deactivate()
                invoke.reject("backend '$requestedBackend' is not available on Android")
                return@runOnUiThread
            }
            if (requestedBackend == "libvlc") {
                startVlc(
                    args,
                    invoke,
                    AtomicBoolean(false),
                    generation,
                )
                return@runOnUiThread
            }
            val playerAllocator = DefaultAllocator(true, 64 * 1024)
            snapshotState.allocator = playerAllocator
            val loadControlBuilder = DefaultLoadControl.Builder().setAllocator(playerAllocator)
            resolveRequestedBufferDurations(args)?.let { durations ->
                loadControlBuilder.setBufferDurationsMs(
                    durations.minMs,
                    durations.maxMs,
                    durations.playbackMs,
                    durations.rebufferMs,
                )
            }
            resolveRequestedTargetBufferBytes(args.targetBufferBytes)?.let {
                loadControlBuilder.setTargetBufferBytes(it)
            }
            val loadControl = loadControlBuilder.build()
            // Several Amlogic TV firmwares advertise Dolby Vision profile 7
            // decoders that open successfully but render black. Profile 7 has
            // a standards-compliant HEVC base layer, so select the HEVC codec
            // directly and keep the zero-copy SurfaceView path.
            val codecSelector = MediaCodecSelector { mimeType, secure, tunneling ->
                val decoderMime = if (
                    mimeType == MimeTypes.VIDEO_DOLBY_VISION
                    && args.dolbyVisionMode != "platform"
                ) {
                    MimeTypes.VIDEO_H265
                } else mimeType
                MediaCodecSelector.DEFAULT.getDecoderInfos(decoderMime, secure, tunneling)
            }
            val renderersFactory = DefaultRenderersFactory(activity)
                .setMediaCodecSelector(codecSelector)
                .setEnableDecoderFallback(args.decoderFallback ?: true)
            val requestHeaders = HashMap(args.headers)
            args.cookies?.takeIf(String::isNotBlank)?.let { requestHeaders["Cookie"] = it }
            args.referrer?.takeIf(String::isNotBlank)?.let { requestHeaders["Referer"] = it }
            val httpFactory = try {
                createHttpDataSourceFactory(args, requestHeaders, bundledCaFile)
            } catch (error: Exception) {
                root.visibility = View.GONE
                nativeSurfaceSync.deactivate()
                invoke.reject(error.message ?: "Could not configure the HTTPS media source")
                return@runOnUiThread
            }
            val dataSourceFactory = DefaultDataSource.Factory(activity, httpFactory)
            val mediaSourceFactory = DefaultMediaSourceFactory(dataSourceFactory)
            val trackSelector = DefaultTrackSelector(activity).apply {
                parameters = buildUponParameters()
                    .setTunnelingEnabled(args.tunneling ?: false)
                    .build()
            }
            val player = ExoPlayer.Builder(activity)
                .setRenderersFactory(renderersFactory)
                .setMediaSourceFactory(mediaSourceFactory)
                .setTrackSelector(trackSelector)
                .setLoadControl(loadControl)
                .build()
            nativePlayer = player
            view.player = player
            player.volume = if (args.muted) 0f else args.volume.toFloat().coerceIn(0f, 1f)
            player.addAnalyticsListener(object : AnalyticsListener {
                override fun onVideoDecoderInitialized(
                    eventTime: AnalyticsListener.EventTime,
                    decoderName: String,
                    initializedTimestampMs: Long,
                    initializationDurationMs: Long,
                ) {
                    snapshotState.videoDecoderName = decoderName
                }
            })
            val resolved = AtomicBoolean(false)
            var playerReady = false
            var firstFrameRendered = false
            fun resolveWhenRenderable() {
                if (playerReady
                    && (firstFrameRendered || !args.autoplay)
                    && resolved.compareAndSet(false, true)
                ) {
                    invoke.resolve(nativeSnapshot(player, snapshotState))
                }
            }
            fun rejectMedia3(message: String) {
                if (resolved.compareAndSet(false, true)) {
                    invoke.reject(message)
                    closeNativePlayer()
                }
            }
            player.addListener(object : Player.Listener {
                override fun onPlaybackStateChanged(state: Int) {
                    if (state == Player.STATE_READY) {
                        // onTracksChanged normally arrives before READY, but initialize from
                        // the authoritative player state so the first resolved snapshot can
                        // never expose an empty cache because of callback ordering.
                        refreshNativeTracks(player.currentTracks)
                        playerReady = true
                        resolveWhenRenderable()
                    } else if (state == Player.STATE_ENDED) {
                        // ExoPlayer keeps playWhenReady armed at EOF. Clear it
                        // so a later seek behaves like an ended HTML video:
                        // update the frame, but stay paused until play() is
                        // explicitly requested.
                        player.playWhenReady = false
                        setPlaybackActive(false)
                    }
                }

                override fun onRenderedFirstFrame() {
                    firstFrameRendered = true
                    resolveWhenRenderable()
                }

                override fun onPlayerError(error: PlaybackException) {
                    rejectMedia3(error.message ?: "Media3 playback failed")
                }

                override fun onVideoSizeChanged(size: VideoSize) {
                    snapshotState.videoSize = size
                }

                override fun onTracksChanged(tracks: Tracks) {
                    refreshNativeTracks(tracks)
                }
            })
            val mediaItem = MediaItem.fromUri(args.uri)
            val startPositionMs = args.startPositionSeconds
                ?.takeIf { it.isFinite() && it >= 0.0 }
                ?.times(1_000.0)
                ?.toLong()
            if (startPositionMs == null) player.setMediaItem(mediaItem)
            else player.setMediaItem(mediaItem, startPositionMs)
            if (args.autoplay && setPlaybackActive(true)) {
                // Arm autoplay while the player is still buffering. Media3 will
                // begin immediately on READY instead of waiting for a second
                // manual play transition.
                player.playWhenReady = true
            }
            player.prepare()
        }
    }

    @Command
    fun controlNative(invoke: Invoke) {
        val args = invoke.parseArgs(NativeControlArgs::class.java)
        activity.runOnUiThread {
            if (args.sessionKey != activeSessionKey) {
                invoke.reject("native player session is stale")
                return@runOnUiThread
            }
            val player = nativePlayer
            val vlc = vlcPlayer
            if (player == null && vlc == null) {
                invoke.reject("native player is not open")
                return@runOnUiThread
            }
            try {
                when (args.action) {
                    "play" -> {
                        check(setPlaybackActive(true)) { "audio focus was denied" }
                        if (player != null) {
                            if (player.playbackState == Player.STATE_ENDED) {
                                player.seekToDefaultPosition()
                            }
                            player.play()
                        } else vlc?.play()
                    }
                    "pause" -> {
                        player?.pause() ?: vlc?.pause()
                        setPlaybackActive(false)
                    }
                    "seek" -> if (player != null) player.seekTo((args.value * 1000.0).toLong())
                        else vlc?.seekTo((args.value * 1000.0).toLong())
                    "volume" -> if (player != null) {
                        player.volume = args.value.toFloat().coerceIn(0f, 1f)
                    } else vlc?.setVolume(args.value.toFloat())
                    "track" -> if (player != null) selectNativeTrack(player, args.index)
                        else vlc?.selectTrack(args.index)
                    "deselectTrack" -> if (player != null) deselectNativeTrack(player, args.index)
                        else vlc?.deselectTrack(args.index)
                    "fit", "crop", "stretch" -> if (player != null) {
                        nativeView?.resizeMode = when (args.action) {
                            "crop" -> AspectRatioFrameLayout.RESIZE_MODE_ZOOM
                            "stretch" -> AspectRatioFrameLayout.RESIZE_MODE_FILL
                            else -> AspectRatioFrameLayout.RESIZE_MODE_FIT
                        }
                    } else vlc?.setFit(args.action)
                    "zoom" -> if (player != null) {
                        nativeView?.videoSurfaceView?.apply {
                            val zoom = args.value.toFloat().coerceIn(1f, 2f)
                            scaleX = zoom
                            scaleY = zoom
                        }
                    } else {
                        vlc?.setZoom(args.value.toFloat())
                    }
                    else -> throw IllegalArgumentException("unknown native action ${args.action}")
                }
                invoke.resolve(activeNativeSnapshot())
            } catch (error: Exception) {
                invoke.reject(error.message ?: "native playback command failed")
            }
        }
    }

    @Command
    fun layoutNative(invoke: Invoke) {
        val args = invoke.parseArgs(NativeLayoutArgs::class.java)
        activity.runOnUiThread {
            if (args.sessionKey != activeSessionKey) {
                invoke.reject("native player session is stale")
                return@runOnUiThread
            }
            nativeSurfaceSync.applyLayout(
                args.x,
                args.y,
                args.width,
                args.height,
                args.scrollX,
                args.scrollY,
            )
            invoke.resolve()
        }
    }

    @Command
    fun statsNative(invoke: Invoke) {
        val args = invoke.parseArgs(NativeSessionArgs::class.java)
        activity.runOnUiThread {
            if (args.sessionKey != activeSessionKey) {
                invoke.reject("native player session is stale")
                return@runOnUiThread
            }
            val player = nativePlayer
            val vlc = vlcPlayer
            if (player == null && vlc == null) invoke.reject("native player is not open")
            else player?.playerError?.let { invoke.reject(it.message ?: "Media3 playback failed") }
                ?: invoke.resolve(activeNativeSnapshot())
        }
    }

    @Command
    fun closeNative(invoke: Invoke) {
        val args = invoke.parseArgs(NativeSessionArgs::class.java)
        activity.runOnUiThread {
            if (args.sessionKey == activeSessionKey) closeNativePlayer()
            invoke.resolve()
        }
    }

    private fun setPlaybackActive(playing: Boolean): Boolean {
        if (!playing) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                focusRequest?.let(audioManager::abandonAudioFocusRequest)
                focusRequest = null
            } else {
                @Suppress("DEPRECATION")
                audioManager.abandonAudioFocus(null)
            }
            activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            return true
        }
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                        .build()
                )
                .setOnAudioFocusChangeListener { change ->
                    if (change == AudioManager.AUDIOFOCUS_LOSS
                        || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT
                    ) {
                        activity.runOnUiThread {
                            nativePlayer?.pause()
                            vlcPlayer?.pause()
                        }
                    }
                }
                .build()
            focusRequest = request
            audioManager.requestAudioFocus(request)
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(null, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN)
        }
        return if (result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            true
        } else false
    }

    private fun selectNativeTrack(player: ExoPlayer, target: Int) {
        val (group, track) = trackTargets[target] ?: return
        player.trackSelectionParameters = player.trackSelectionParameters
            .buildUpon()
            .setTrackTypeDisabled(group.type, false)
            .clearOverridesOfType(group.type)
            .addOverride(TrackSelectionOverride(group.mediaTrackGroup, track))
            .build()
        refreshNativeTracks(player.currentTracks)
    }

    private fun deselectNativeTrack(player: ExoPlayer, target: Int) {
        val (group, _) = trackTargets[target] ?: return
        player.trackSelectionParameters = player.trackSelectionParameters
            .buildUpon()
            .setTrackTypeDisabled(group.type, true)
            .build()
        refreshNativeTracks(player.currentTracks)
    }

    private fun startVlc(
        args: NativeOpenArgs,
        invoke: Invoke,
        resolved: AtomicBoolean,
        generation: Int,
    ) {
        if (generation != openGeneration || vlcPlayer != null) return
        val root = nativeRoot
        val vlcLayout = vlcView
        if (root == null || vlcLayout == null) {
            root?.visibility = View.GONE
            nativeSurfaceSync.deactivate()
            if (resolved.compareAndSet(false, true)) invoke.reject("LibVLC video surface is unavailable")
            return
        }
        clearVlcTrackCache()
        nativeView?.visibility = View.GONE
        vlcLayout.visibility = View.VISIBLE
        root.visibility = View.VISIBLE
        val vlc = try {
            VlcPlayer(
                activity,
                vlcLayout,
                VlcPlayerConfig(
                    uri = args.uri,
                    autoplay = args.autoplay,
                    initialVolume = if (args.muted) 0f else args.volume.toFloat().coerceIn(0f, 1f),
                    startPositionMs = args.startPositionSeconds?.times(1_000.0)?.toLong(),
                    networkCachingMs = args.minBufferMs?.coerceIn(1_000, 120_000),
                    userAgent = args.userAgent,
                    referrer = args.referrer,
                    cookies = args.cookies,
                    caFile = resolveCaFile(args, bundledCaFile),
                ),
            )
        } catch (error: Throwable) {
            Log.e("TauriVideo", "LibVLC backend failed to initialize", error)
            vlcLayout.visibility = View.GONE
            root.visibility = View.GONE
            nativeSurfaceSync.deactivate()
            if (resolved.compareAndSet(false, true)) {
                invoke.reject(
                    "LibVLC startup failed: ${error.javaClass.simpleName}: " +
                        (error.message ?: "no diagnostic message")
                )
            }
            return
        }
        vlcPlayer = vlc
        vlcLayout.post {
            if (generation != openGeneration || vlcPlayer !== vlc) return@post
            vlc.open(
                onRenderable = {
                    activity.runOnUiThread {
                        if (generation != openGeneration || vlcPlayer !== vlc) return@runOnUiThread
                        if (args.autoplay) setPlaybackActive(true)
                        if (resolved.compareAndSet(false, true)) {
                            invoke.resolve(activeNativeSnapshot())
                        }
                    }
                },
                onError = { vlcFailure ->
                    activity.runOnUiThread {
                        if (generation != openGeneration || vlcPlayer !== vlc) return@runOnUiThread
                        vlc.release()
                        vlcPlayer = null
                        vlcLayout.visibility = View.GONE
                        root.visibility = View.GONE
                        nativeSurfaceSync.deactivate()
                        if (resolved.compareAndSet(false, true)) {
                            invoke.reject(vlcFailure)
                        }
                    }
                },
            )
        }
    }

    private fun activeNativeSnapshot(): JSObject {
        nativePlayer?.let { return nativeSnapshot(it, snapshotState) }
        val vlc = vlcPlayer ?: error("native player is not open")
        return vlcSnapshot(vlc.snapshot(snapshotState.nativeContainer), snapshotState)
    }

    private fun clearVlcTrackCache() {
        snapshotState.cachedVlcTrackSource = null
        snapshotState.cachedVlcTracks = JSArray()
    }

    private fun refreshNativeTracks(tracks: Tracks) {
        val cachedTracks = ArrayList<JSObject>()
        trackTargets.clear()
        var target = 0
        tracks.groups.forEach { group ->
            val kind = when (group.type) {
                C.TRACK_TYPE_VIDEO -> "video"
                C.TRACK_TYPE_AUDIO -> "audio"
                C.TRACK_TYPE_TEXT -> "subtitle"
                else -> return@forEach
            }
            for (index in 0 until group.length) {
                val format: Format = group.getTrackFormat(index)
                val language = format.language ?: "und"
                trackTargets[target] = group to index
                cachedTracks.add(JSObject().apply {
                    put("id", target.toString())
                    put("index", target)
                    put("kind", kind)
                    put("language", language)
                    put("label", format.label ?: if (language == "und") kind else language.uppercase())
                    put("codec", format.codecs ?: format.sampleMimeType?.substringAfter('/') ?: "")
                    put("selected", group.isTrackSelected(index))
                })
                target += 1
            }
        }
        snapshotState.cachedNativeTracks = JSArray(cachedTracks)
    }

    private fun clearNativeTrackCache() {
        trackTargets.clear()
        snapshotState.cachedNativeTracks = JSArray()
    }

    private fun closeNativePlayer() {
        openGeneration += 1
        activeSessionKey = null
        setPlaybackActive(false)
        nativeView?.player = null
        nativePlayer?.release()
        nativePlayer = null
        vlcPlayer?.release()
        vlcPlayer = null
        nativeView?.visibility = View.VISIBLE
        vlcView?.visibility = View.GONE
        nativeRoot?.visibility = View.GONE
        nativeSurfaceSync.deactivate()
        snapshotState.allocator = null
        snapshotState.videoDecoderName = "uninitialized"
        snapshotState.lastRenderedFrames = 0L
        snapshotState.lastFrameSampleNs = 0L
        snapshotState.measuredFps = 0.0
        snapshotState.nativeContainer = "unknown"
        clearNativeTrackCache()
        clearVlcTrackCache()
        snapshotState.videoSize = VideoSize.UNKNOWN
    }
}
