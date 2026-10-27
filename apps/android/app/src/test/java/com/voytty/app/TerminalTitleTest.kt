package com.voytty.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TerminalTitleTest {
    @Test fun programTitleIsShownOnlyWhenItAddsToTheManualName() {
        val manual = "开发会话"
        assertEquals("编辑器", terminalApplicationTitle(manual, "编辑器"))
        assertNull(terminalApplicationTitle(manual, ""))
        assertNull(terminalApplicationTitle(manual, manual))
        assertEquals("编辑器", terminalApplicationTitle("重命名", "编辑器"))
    }
}
