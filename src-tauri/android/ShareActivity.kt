// #1759 şeffaf paylaşım trampleni: Paylaş menüsündeki üç giriş (activity-alias) buraya gelir.
// WebView YOK (Tauri aktivitesi değil): SEND intent'ini okuyup singleTask MainActivity'ye iletir ve
// `finish()` olur. Kullanıcı bu sürede paylaştığı uygulamayı (Instagram) görmeye devam eder; hızlı
// gönderim MainActivity'nin tek WebView'ında, saydam kökle yapılır.
// Neden WebView'sız: Tauri 2.11 `PluginManager` ilk aktiviteye bir kez bağlanır ve her eklentiyi bir
// kez `load()` eder — ikinci bir TauriActivity'nin WebView'ı paylaş köprüsüne hiç bağlanmaz, `done`
// yanlış görevi kapatır. Dex açıkken gelen paylaşım da bu yüzden MainActivity'e (`onNewIntent`) gider.
// Süre ölçümü: `onCreate` damgası (`dex.share_t0`) iletilir; ShareTargetPlugin `done`'da yalnız süreyi
// loglar (içerik yok). Görsel URI izni (`FLAG_GRANT_READ_URI_PERMISSION`) iletide taşınır, yoksa
// MainActivity paylaşılan görseli okuyamaz.

package com.hakunokina.dex

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.SystemClock

class ShareActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        forward(intent)
        finish()
    }

    private fun forward(src: Intent?) {
        if (src == null || src.action != Intent.ACTION_SEND) return
        // Alias adı MainActivity component'ine çevrilince kaybolur; görev extra ile taşınır.
        val task = when (src.component?.className?.substringAfterLast('.')) {
            "ShareRepo" -> "repo"
            "ShareVerify" -> "verify"
            else -> "ask"
        }
        val dst = Intent(src).apply {
            setClassName(this@ShareActivity, "com.hakunokina.dex.MainActivity")
            putExtra(EXTRA_TASK, task)
            if (!hasExtra(EXTRA_T0)) putExtra(EXTRA_T0, SystemClock.elapsedRealtime())
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            startActivity(dst)
        } catch (e: Exception) {
            // Hedef yoksa sessizce kapanır; kullanıcı paylaştığı uygulamada kalır.
        }
    }

    companion object {
        /** ShareTargetPlugin.taskOf'un önce okuduğu görev (`repo | verify | ask`). */
        const val EXTRA_TASK = "dex.task"
        /** Tramplen `onCreate` damgası (ms, `SystemClock.elapsedRealtime`); yalnız süre ölçümü. */
        const val EXTRA_T0 = "dex.share_t0"
    }
}
