package io.github.leonfox28.zterm

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Typeface
import android.util.TypedValue
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test

class TerminalFontTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test fun packagedFaceHasEqualAsciiBoxAndNerdIconAdvancesAtEverySize() {
        val typeface = TerminalFont.typeface(context)
        assertNotEquals("terminal does not resolve the OEM monospace alias", Typeface.MONOSPACE, typeface)
        // Powerline, Seti, Devicons, Font Awesome, Octicons, Codicons, Weather,
        // Pomicons and Material Design (a supplementary-plane private-use code point).
        val icons = listOf("\ue0a0", "\ue0b0", "\ue5fa", "\ue7a8", "\uf115", "\uf418",
            "\uea60", "\ue300", "\ue000", "\udb80\udc01")
        val glyphs = (' '..'~').map { it.toString() } + icons + listOf("─", "│", "┌", "┘", "●", "e\u0301")
        for (size in terminalFontSizes) {
            val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
                this.typeface = typeface
                textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, size.toFloat(), context.resources.displayMetrics)
            }
            val width = paint.measureText("M")
            for (glyph in glyphs) {
                assertTrue("packaged font contains $glyph at $size sp", paint.hasGlyph(glyph))
                assertEquals("$glyph occupies one cell at $size sp", width, paint.measureText(glyph), .01f)
            }
        }
    }

    @Test fun overwideFallbackIsFittedWithoutLosingInkOrMovingTheNextCell() {
        val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            typeface = TerminalFont.typeface(context); textSize = 64f; color = Color.WHITE
        }
        for (text in listOf("界", "😀", "e\u0301", "\udb80\udc01")) {
            // Force a width mismatch, regardless of the device's fallback metrics.
            val advance = paint.measureText(text)
            val width = advance / 2
            val scaled = Bitmap.createBitmap(256, 120, Bitmap.Config.ARGB_8888)
            val reference = Bitmap.createBitmap(256, 120, Bitmap.Config.ARGB_8888)
            try {
                val canvas = Canvas(scaled)
                drawTerminalGlyph(canvas, text, 16f, 80f, width, paint)
                canvas.drawRect(16f + width, 0f, 18f + width, 120f, paint)
                val expected = Canvas(reference)
                val fittedPaint = Paint(paint).apply { textScaleX = width / advance }
                expected.drawText(text, 16f, 80f, fittedPaint)
                expected.drawRect(16f + width, 0f, 18f + width, 120f, paint)
                // Canvas scaling and textScaleX can rasterize differently. Compare
                // ink extents instead of requiring an identical antialias pattern.
                fun inkBounds(bitmap: Bitmap): android.graphics.Rect {
                    val bounds = android.graphics.Rect()
                    for (y in 0 until bitmap.height) for (x in 0 until bitmap.width)
                        if (Color.alpha(bitmap.getPixel(x, y)) > 0) bounds.union(x, y, x + 1, y + 1)
                    return bounds
                }
                val actualBounds = inkBounds(scaled)
                val expectedBounds = inkBounds(reference)
                assertTrue("$text retains its glyph", actualBounds.width() > 2)
                assertEquals("$text fits without shifting the next cell", expectedBounds, actualBounds)
                assertEquals("fitting leaves Paint reusable", 1f, paint.textScaleX, 0f)
            } finally { scaled.recycle(); reference.recycle() }
        }
    }
}
