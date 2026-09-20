package io.github.taurivideo.plugin

import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.datasource.HttpDataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import java.io.ByteArrayInputStream
import java.io.File
import java.security.KeyStore
import java.security.cert.CertificateException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager
import okhttp3.OkHttpClient

/**
 * Custom TLS CA clients, keyed by bundle fingerprint. The map is an
 * access-ordered LRU that holds at most four clients, so switching between
 * many bundles reuses the recent ones without leaking sockets.
 */
private val customCaClients = object : LinkedHashMap<String, OkHttpClient>(4, 0.75f, true) {
    override fun removeEldestEntry(
        eldest: MutableMap.MutableEntry<String, OkHttpClient>?,
    ): Boolean = size > 4
}

internal fun resolveCaFile(args: NativeOpenArgs, bundledCaFile: File?): File? =
    when (val requested = args.tlsCaFile?.trim()) {
        null, "" -> null
        "bundled" -> bundledCaFile
            ?: throw IllegalArgumentException("Bundled TLS CA file is unavailable to the Android app")
        else -> File(requested).takeIf { it.isFile }
            ?: throw IllegalArgumentException("TLS CA file is unavailable to the Android app")
    }

/** Build an HTTPS source with an explicit CA bundle without disabling hostname validation. */
internal fun createHttpDataSourceFactory(
    args: NativeOpenArgs,
    requestHeaders: Map<String, String>,
    bundledCaFile: File?,
): HttpDataSource.Factory {
    val caFile = resolveCaFile(args, bundledCaFile)
    if (caFile == null) {
        val factory = DefaultHttpDataSource.Factory()
            .setAllowCrossProtocolRedirects(true)
            .setDefaultRequestProperties(requestHeaders)
        args.userAgent?.takeIf(String::isNotBlank)?.let(factory::setUserAgent)
        return factory
    }

    val cacheKey = "${caFile.canonicalPath}:${caFile.length()}:${caFile.lastModified()}"
    val client = customCaClients.getOrPut(cacheKey) { buildCustomCaClient(caFile) }
    val factory = OkHttpDataSource.Factory(client)
        .setDefaultRequestProperties(requestHeaders)
    args.userAgent?.takeIf(String::isNotBlank)?.let(factory::setUserAgent)
    return factory
}

private fun buildCustomCaClient(caFile: File): OkHttpClient {
    val certificateFactory = CertificateFactory.getInstance("X.509")
    val certificates = PEM_CERTIFICATE.findAll(caFile.readText()).map { match ->
        ByteArrayInputStream(match.value.toByteArray(Charsets.US_ASCII)).use {
            certificateFactory.generateCertificate(it)
        }
    }.toList()
    require(certificates.isNotEmpty()) { "TLS CA bundle contains no certificates" }
    val keyStore = KeyStore.getInstance(KeyStore.getDefaultType()).apply {
        load(null)
        certificates.forEachIndexed { index, certificate ->
            setCertificateEntry("tauri-video-ca-$index", certificate)
        }
    }
    val trustManagerFactory = TrustManagerFactory
        .getInstance(TrustManagerFactory.getDefaultAlgorithm())
        .apply { init(keyStore) }
    val customTrustManager = trustManagerFactory.trustManagers
        .filterIsInstance<X509TrustManager>()
        .singleOrNull()
        ?: error("TLS CA bundle did not create an X.509 trust manager")
    val systemTrustManager = TrustManagerFactory
        .getInstance(TrustManagerFactory.getDefaultAlgorithm())
        .apply { init(null as KeyStore?) }
        .trustManagers
        .filterIsInstance<X509TrustManager>()
        .singleOrNull()
        ?: error("Android did not provide a system X.509 trust manager")
    val trustManager = CompositeTrustManager(systemTrustManager, customTrustManager)
    val sslContext = SSLContext.getInstance("TLS").apply {
        init(null, arrayOf(trustManager), null)
    }
    return OkHttpClient.Builder()
        .sslSocketFactory(sslContext.socketFactory, trustManager)
        .followRedirects(true)
        .followSslRedirects(true)
        .retryOnConnectionFailure(true)
        .build()
}

private val PEM_CERTIFICATE = Regex(
    "-----BEGIN CERTIFICATE-----[\\s\\S]+?-----END CERTIFICATE-----"
)

private class CompositeTrustManager(
    private val system: X509TrustManager,
    private val custom: X509TrustManager,
) : X509TrustManager {
    override fun getAcceptedIssuers(): Array<X509Certificate> =
        system.acceptedIssuers + custom.acceptedIssuers

    override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {
        system.checkClientTrusted(chain, authType)
    }

    override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
        try {
            system.checkServerTrusted(chain, authType)
        } catch (systemError: CertificateException) {
            try {
                custom.checkServerTrusted(chain, authType)
            } catch (customError: CertificateException) {
                customError.addSuppressed(systemError)
                throw customError
            }
        }
    }
}
