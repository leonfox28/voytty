package io.github.leonfox28.voytty

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import android.graphics.text.TextRunShaper
import android.util.TypedValue
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test

class TerminalFontTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test fun packagedFacesHaveOneAndTwoCellAdvancesAtEverySize() {
        // Powerline, Seti, Devicons, Font Awesome, Octicons, Codicons, Weather,
        // Pomicons and Material Design (a supplementary-plane private-use code point).
        val icons = listOf("\ue0a0", "\ue0b0", "\ue5fa", "\ue7a8", "\uf115", "\uf418",
            "\uea60", "\ue300", "\ue000", "\udb80\udc01")
        val glyphs = (' '..'~').map { it.toString() } + icons + listOf("─", "│", "┌", "┘", "●", "e\u0301")
        for (size in terminalFontSizes) for (style in 0..3) {
            val paint = Paint(TerminalFont.paintFlags).apply {
                typeface = TerminalFont.typeface(context, style)
                textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, size.toFloat(), context.resources.displayMetrics)
            }
            assertNotEquals("terminal does not resolve the OEM monospace alias", Typeface.MONOSPACE, paint.typeface)
            val width = paint.terminalAdvance("M")
            val normal = Paint(paint).apply { typeface = TerminalFont.typeface(context) }
            assertEquals("style $style preserves the grid width at $size sp", normal.terminalAdvance("M"), width, .01f)
            assertEquals("style $style preserves the baseline", normal.fontMetrics.ascent, paint.fontMetrics.ascent, .01f)
            assertEquals("style $style preserves the row height", normal.fontMetrics.descent, paint.fontMetrics.descent, .01f)
            for (glyph in glyphs) {
                assertTrue("$glyph is available in style $style at $size sp", paint.hasGlyph(glyph))
                assertEquals("$glyph occupies one cell in style $style at $size sp", width, paint.terminalAdvance(glyph), .01f)
            }
            for (glyph in "中文终端你好世界字体设置简体繁體，。！？") {
                val text = glyph.toString()
                assertTrue("$glyph is available in style $style", paint.hasGlyph(text))
                assertEquals("$glyph occupies two cells in style $style at $size sp", width * 2, paint.terminalAdvance(text), .01f)
                val bounds = Rect()
                paint.getTextBounds(text, 0, text.length, bounds)
                assertFalse("$glyph has visible ink", bounds.isEmpty)
                assertTrue("$glyph fits vertically at $size sp in style $style",
                    bounds.top >= kotlin.math.floor(paint.fontMetrics.ascent).toInt() &&
                        bounds.bottom <= kotlin.math.ceil(paint.fontMetrics.descent).toInt())
            }
        }
    }

    // Inspect the actual fonts selected by Android; hasGlyph alone includes system fallback.
    @Test @SdkSuppress(minSdkVersion = 31)
    fun chineseAndIconsResolveToEachPackagedFaceWithoutFallback() {
        val resources = listOf(R.font.terminal_mono, R.font.terminal_mono_bold,
            R.font.terminal_mono_italic, R.font.terminal_mono_bold_italic)
        val text = "Mi中文终端你好世界字体设置简体繁體，。！？─\ue0a0\ue0b0\uf115\ue7a8\udb80\udc01"
        resources.forEachIndexed { style, resource ->
            val paint = Paint(TerminalFont.paintFlags).apply { typeface = TerminalFont.typeface(context, style); textSize = 32f }
            val expected = android.graphics.fonts.Font.Builder(context.resources, resource).build()
            val glyphs = TextRunShaper.shapeTextRun(text, 0, text.length, 0, text.length, 0f, 0f, false, paint)
            assertTrue("text produces glyphs", glyphs.glyphCount() > 0)
            for (index in 0 until glyphs.glyphCount()) {
                assertNotEquals("no missing glyph in style $style", 0, glyphs.getGlyphId(index))
            }
            val actualFonts = (0 until glyphs.glyphCount()).map { glyphs.getFont(it) }.toSet()
            for (font in actualFonts) assertEquals("style $style uses packaged font bytes, including Chinese and icons",
                expected.buffer, font.buffer)
        }
    }

    @Test fun overwideFallbackIsFittedWithoutLosingInkOrMovingTheNextCell() {
        val paint = Paint(TerminalFont.paintFlags).apply {
            typeface = TerminalFont.typeface(context); textSize = 64f; color = Color.WHITE
        }
        for (text in listOf("界", "😀", "e\u0301", "\udb80\udc01")) {
            // Force a width mismatch, regardless of the device's fallback metrics.
            val advance = paint.terminalAdvance(text)
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
