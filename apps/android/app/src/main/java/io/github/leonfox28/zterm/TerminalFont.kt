package io.github.leonfox28.zterm

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily

/** One packaged face owns terminal measurement, glyphs and the settings preview. */
internal object TerminalFont {
    val family = FontFamily(Font(R.font.terminal_mono))

    fun typeface(context: Context): Typeface = context.resources.getFont(R.font.terminal_mono)
}

/** System fallback glyphs (notably emoji) can exceed the host's one/two-cell allocation. */
internal fun drawTerminalGlyph(canvas: Canvas, text: String, x: Float, baseline: Float,
                               width: Float, paint: Paint) {
    // A mark on a blank cell has no base glyph anchor. Some fonts place all its
    // ink before the origin, where the cell clip would hide it completely.
    if (text.length > 1 && text[0] == ' ') {
        val bounds = Rect()
        paint.getTextBounds(text, 0, text.length, bounds)
        if (!bounds.isEmpty) {
            val scale = minOf(1f, width / bounds.width())
            canvas.save()
            canvas.translate(x + (width - bounds.width() * scale) / 2 - bounds.left * scale, baseline)
            canvas.scale(scale, 1f)
            canvas.drawText(text, 0f, 0f, paint)
            canvas.restore()
            return
        }
    }
    val advance = paint.measureText(text)
    if (advance > width) {
        canvas.save()
        canvas.translate(x, baseline)
        canvas.scale(width / advance, 1f)
        canvas.drawText(text, 0f, 0f, paint)
        canvas.restore()
    } else canvas.drawText(text, x, baseline, paint)
}
