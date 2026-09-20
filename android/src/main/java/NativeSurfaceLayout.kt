package io.github.taurivideo.plugin

import android.view.View
import android.view.ViewTreeObserver
import android.webkit.WebView
import android.widget.FrameLayout

/**
 * Synchronizes the native playback plane with the WebView's document
 * position. The plugin supplies the current native root and WebView through
 * providers because both are installed at different lifecycle points.
 */
internal class NativeSurfaceSync(
    private val root: () -> View?,
    private val webView: () -> WebView?,
) {
    private var documentX = 0.0
    private var documentY = 0.0
    private var layoutActive = false
    private var synchronizerRegistered = false
    private val preDraw = ViewTreeObserver.OnPreDrawListener {
        syncPosition()
        true
    }

    fun applyLayout(
        x: Double,
        y: Double,
        width: Double,
        height: Double,
        scrollX: Double,
        scrollY: Double,
    ) {
        val view = root() ?: return
        val params = (view.layoutParams as? FrameLayout.LayoutParams) ?: FrameLayout.LayoutParams(1, 1)
        val nextWidth = width.toInt().coerceAtLeast(1)
        val nextHeight = height.toInt().coerceAtLeast(1)
        if (params.width != nextWidth || params.height != nextHeight
            || params.leftMargin != 0 || params.topMargin != 0
        ) {
            params.width = nextWidth
            params.height = nextHeight
            params.leftMargin = 0
            params.topMargin = 0
            view.layoutParams = params
        }
        documentX = x + scrollX
        documentY = y + scrollY
        layoutActive = true
        registerIfNeeded()
        syncPosition()
    }

    fun registerIfNeeded() {
        if (!layoutActive || synchronizerRegistered) return
        val observer = webView()?.viewTreeObserver?.takeIf { it.isAlive } ?: return
        observer.addOnPreDrawListener(preDraw)
        synchronizerRegistered = true
    }

    fun unregister() {
        if (!synchronizerRegistered) return
        webView()?.viewTreeObserver?.takeIf { it.isAlive }
            ?.removeOnPreDrawListener(preDraw)
        synchronizerRegistered = false
    }

    fun deactivate() {
        layoutActive = false
        unregister()
    }

    private fun syncPosition() {
        if (!layoutActive) return
        val view = root() ?: return
        val webView = webView() ?: return
        val nextX = (documentX - webView.scrollX).toFloat()
        val nextY = (documentY - webView.scrollY).toFloat()
        if (view.translationX != nextX) view.translationX = nextX
        if (view.translationY != nextY) view.translationY = nextY
    }
}
