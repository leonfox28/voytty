package com.voytty.app

import android.app.ActivityManager
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Exercise Android's real service scheduling without needing a remote host. */
class BackgroundConnectionStartRaceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val app = ApplicationProvider.getApplicationContext<VoyttyApplication>()
    private fun main(action: () -> Unit) = instrumentation.runOnMainSync(action)

    @Suppress("DEPRECATION")
    private fun foreground(): Boolean = app.getSystemService(ActivityManager::class.java)
        .getRunningServices(100).any {
            it.service.className == TerminalConnectionService::class.java.name && it.foreground
        }

    @Test fun cancellingAPendingForegroundStartDoesNotCrashTheProcess() = runBlocking {
        val repository = app.repository
        withTimeout(10_000) { while (!repository.state.value.initialized) delay(20) }
        main { repository.goHome() }
        withTimeout(10_000) { while (repository.state.value.hostId != null) delay(20) }
        ActivityScenario.launch(MainActivity::class.java).use {
            val owner = repository.backgroundConnection
            val wanted = BackgroundConnectionState(wanted = true, phase = "connecting")
            try {
                repeat(32) {
                    // A fast occupied/error result can cancel connecting in
                    // the same main-thread turn, before any service callback.
                    main {
                        owner.update(wanted, appVisible = true)
                        owner.update(wanted.copy(wanted = false), appVisible = true)
                    }
                    delay(100)
                }
                delay(12_000)
                assertFalse("cancelled connection leaves no foreground service", foreground())
            } finally {
                main { owner.update(wanted.copy(wanted = false), appVisible = true) }
            }
        }
    }

    @Test fun rapidForegroundStartsSurviveTheirPlatformWatchdog() = runBlocking {
        val repository = app.repository
        withTimeout(10_000) { while (!repository.state.value.initialized) delay(20) }
        main { repository.goHome() }
        withTimeout(10_000) { while (repository.state.value.hostId != null) delay(20) }
        ActivityScenario.launch(MainActivity::class.java).use {
            val owner = repository.backgroundConnection
            val wanted = BackgroundConnectionState(wanted = true, phase = "connecting")
            try {
                main { owner.update(wanted, appVisible = true) }
                withTimeout(10_000) { while (!foreground()) delay(20) }
                repeat(256) { index ->
                    // Android may deliver the new start to the same instance
                    // after stopForeground, before its queued destruction.
                    main {
                        owner.update(wanted.copy(wanted = false), appVisible = true)
                        owner.update(wanted, appVisible = true)
                    }
                    delay(listOf(1L, 5L, 20L, 30L)[index % 4])
                }
                // Keep the process alive beyond the platform's start watchdog;
                // stopping it immediately would hide the original crash.
                delay(12_000)
                assertFalse(owner.failed.value)
                assertTrue("the latest foreground start must be promoted", foreground())
            } finally {
                main { owner.update(wanted.copy(wanted = false), appVisible = true) }
            }
        }
    }
}
