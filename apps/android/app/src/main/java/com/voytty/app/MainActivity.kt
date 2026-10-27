package com.voytty.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.core.view.ViewCompat

class MainActivity : ComponentActivity() {
    override fun onStart() {
        super.onStart()
        (application as VoyttyApplication).repository.activityStarted()
        (application as VoyttyApplication).diagnostics.record(com.voytty.app.nativebridge.AppDiagnostic.FOREGROUND)
    }
    override fun onStop() {
        (application as VoyttyApplication).diagnostics.record(com.voytty.app.nativebridge.AppDiagnostic.BACKGROUND)
        (application as VoyttyApplication).repository.activityStopped()
        super.onStop()
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val imeAnimation = ImeAnimationState()
        ViewCompat.setWindowInsetsAnimationCallback(window.decorView, imeAnimation)
        val repository = (application as VoyttyApplication).repository
        val updates = (application as VoyttyApplication).updates
        setContent { CompositionLocalProvider(LocalImeAnimation provides imeAnimation) { VoyttyApp(repository, updates) } }
    }
}
