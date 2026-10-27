package com.voytty.app

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight

/** LXGW's Light/Regular weights provide normal/bold terminal text, with real italics. */
internal object TerminalFont {
    // Independent integer rounding would break the font's 1:2 Latin/CJK advances.
    const val paintFlags = Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG or Paint.LINEAR_TEXT_FLAG

    val family = FontFamily(
        Font(R.font.terminal_mono, FontWeight.Normal),
        Font(R.font.terminal_mono_bold, FontWeight.Bold),
        Font(R.font.terminal_mono_italic, FontWeight.Normal, FontStyle.Italic),
        Font(R.font.terminal_mono_bold_italic, FontWeight.Bold, FontStyle.Italic),
    )

    fun typeface(context: Context, style: Int = Typeface.NORMAL): Typeface = context.resources.getFont(when (style) {
        Typeface.BOLD -> R.font.terminal_mono_bold
        Typeface.ITALIC -> R.font.terminal_mono_italic
        Typeface.BOLD_ITALIC -> R.font.terminal_mono_bold_italic
        else -> R.font.terminal_mono
    })
}

/** Paint.measureText rounds up to whole pixels, adding gaps to a one/two-cell grid. */
internal fun Paint.terminalAdvance(text: String): Float =
    getRunAdvance(text, 0, text.length, 0, text.length, false, text.length)

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
    val advance = paint.terminalAdvance(text)
    // Font advances have subpixel quantization. Do not distort an otherwise
    // matching two-cell glyph for a difference smaller than 1/100 pixel.
    if (advance > width + .01f) {
        canvas.save()
        canvas.translate(x, baseline)
        canvas.scale(width / advance, 1f)
        canvas.drawText(text, 0f, 0f, paint)
        canvas.restore()
    } else canvas.drawText(text, x, baseline, paint)
}
