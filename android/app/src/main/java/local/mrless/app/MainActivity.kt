package local.mrless.app

import android.annotation.SuppressLint
import android.os.Bundle
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.WindowCompat
import java.io.File

class MainActivity : AppCompatActivity() {
    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)

        val dir = File(filesDir, "mrless").apply { mkdirs() }
        if (NativeNode.start(dir.absolutePath, null) != 0) {
            finish()
            return
        }
        val ws = NativeNode.wsPort().toInt().let { if (it == 0) 7422 else it }
        val bootJs =
            "window.__MRLESS_WS='ws://127.0.0.1:$ws';window.__MRLESS_ANDROID=true;"

        val web = WebView(this)
        web.settings.javaScriptEnabled = true
        web.settings.domStorageEnabled = true
        web.setBackgroundColor(0xFF0D0D0D.toInt())
        web.webViewClient = object : WebViewClient() {
            override fun onPageFinished(view: WebView?, url: String?) {
                view?.evaluateJavascript(bootJs, null)
            }

            override fun shouldInterceptRequest(
                view: WebView,
                request: WebResourceRequest
            ): WebResourceResponse? {
                if (request.url.host != "app") return null
                var path = request.url.path ?: "/index.html"
                if (path == "/" || path.isEmpty()) path = "/index.html"
                return try {
                    val stream = assets.open("www$path")
                    val mime = when {
                        path.endsWith(".js") -> "text/javascript"
                        path.endsWith(".css") -> "text/css"
                        path.endsWith(".html") -> "text/html"
                        path.endsWith(".svg") -> "image/svg+xml"
                        path.endsWith(".png") -> "image/png"
                        else -> "application/octet-stream"
                    }
                    WebResourceResponse(mime, "utf-8", stream)
                } catch (_: Exception) {
                    null
                }
            }
        }
        web.loadUrl("https://app/index.html")
        setContentView(web)
    }

    override fun onDestroy() {
        NativeNode.stop()
        super.onDestroy()
    }
}
