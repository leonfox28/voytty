package com.voytty.app

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Semantic colours Material does not name: connection health and key-cap chrome. */
@Immutable internal data class VoyttyExtraColors(val success: Color, val warning: Color, val keyCap: Color, val onKeyCap: Color)

internal val LocalVoyttyColors = staticCompositionLocalOf {
    VoyttyExtraColors(Color(0xFF5BB450), Color(0xFFD99A2B), Color(0xFFECEFEA), Color(0xFF161A17))
}

// Backgrounds match the native terminal palette in crates/android/src/terminal.rs.
private val darkScheme = darkColorScheme(
    primary = Color(0xFFA3E07F), onPrimary = Color(0xFF0E2005),
    primaryContainer = Color(0xFF233A1A), onPrimaryContainer = Color(0xFFC9F4AF),
    secondaryContainer = Color(0xFF26302A), onSecondaryContainer = Color(0xFFDCE6DE),
    background = Color(0xFF101713), onBackground = Color(0xFFE8EDE8),
    surface = Color(0xFF101713), onSurface = Color(0xFFE8EDE8),
    surfaceContainerLowest = Color(0xFF0B110D), surfaceContainerLow = Color(0xFF151D18),
    surfaceContainer = Color(0xFF19221C), surfaceContainerHigh = Color(0xFF222C25),
    surfaceContainerHighest = Color(0xFF2B362E), surfaceVariant = Color(0xFF2B362E),
    onSurfaceVariant = Color(0xFF98A69C), outline = Color(0xFF5D6B61), outlineVariant = Color(0xFF28322B),
    error = Color(0xFFFF8F84), onError = Color(0xFF3A0905),
    errorContainer = Color(0xFF4A1A15), onErrorContainer = Color(0xFFFFDAD5),
)
private val lightScheme = lightColorScheme(
    primary = Color(0xFF39691C), onPrimary = Color.White,
    primaryContainer = Color(0xFFDCF0CC), onPrimaryContainer = Color(0xFF0F2C04),
    secondaryContainer = Color(0xFFE3EAE3), onSecondaryContainer = Color(0xFF1B231D),
    background = Color(0xFFF5F8F3), onBackground = Color(0xFF151A16),
    surface = Color(0xFFF5F8F3), onSurface = Color(0xFF151A16),
    surfaceContainerLowest = Color.White, surfaceContainerLow = Color(0xFFFAFBF9),
    surfaceContainer = Color.White, surfaceContainerHigh = Color(0xFFEAEEE8),
    surfaceContainerHighest = Color(0xFFE1E6DF), surfaceVariant = Color(0xFFE1E6DF),
    onSurfaceVariant = Color(0xFF59655C), outline = Color(0xFF89958C), outlineVariant = Color(0xFFDDE3DB),
    error = Color(0xFFBA1A1A), errorContainer = Color(0xFFFFDAD6), onErrorContainer = Color(0xFF410002),
)
private val darkExtras = VoyttyExtraColors(Color(0xFF7BD66A), Color(0xFFF0B85A), Color(0xFF222C25), Color(0xFFE8EDE8))
private val lightExtras = VoyttyExtraColors(Color(0xFF2E8B37), Color(0xFFB7791F), Color(0xFFE4E9E2), Color(0xFF151A16))

private val typography = Typography().run {
    copy(
        headlineSmall = headlineSmall.copy(fontSize = 26.sp, lineHeight = 32.sp, fontWeight = FontWeight.SemiBold),
        titleLarge = titleLarge.copy(fontSize = 20.sp, lineHeight = 26.sp, fontWeight = FontWeight.SemiBold),
        titleMedium = titleMedium.copy(fontSize = 16.sp, lineHeight = 22.sp, fontWeight = FontWeight.SemiBold),
        bodyLarge = bodyLarge.copy(fontSize = 15.sp, lineHeight = 21.sp),
        bodyMedium = bodyMedium.copy(fontSize = 14.sp, lineHeight = 20.sp),
        bodySmall = bodySmall.copy(fontSize = 12.sp, lineHeight = 16.sp),
        labelMedium = TextStyle(fontSize = 12.sp, lineHeight = 16.sp, fontWeight = FontWeight.SemiBold, letterSpacing = .6.sp),
    )
}
private val shapes = Shapes(
    extraSmall = RoundedCornerShape(6.dp), small = RoundedCornerShape(10.dp), medium = RoundedCornerShape(16.dp),
    large = RoundedCornerShape(22.dp), extraLarge = RoundedCornerShape(28.dp),
)

@Composable internal fun VoyttyTheme(dark: Boolean, content: @Composable () -> Unit) {
    CompositionLocalProvider(LocalVoyttyColors provides if (dark) darkExtras else lightExtras) {
        MaterialTheme(colorScheme = if (dark) darkScheme else lightScheme, typography = typography, shapes = shapes, content = content)
    }
}
