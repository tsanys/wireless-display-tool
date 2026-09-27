package com.wdt.receiver

import com.wdt.receiver.webrtc.AudioFocusController
import com.wdt.receiver.webrtc.AudioFocusSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Uji logika audio focus tanpa device: memastikan focus hanya diambil saat
 * audio aktif dan SELALU dilepas saat nonaktif/release (tidak ada focus
 * tersisa setelah sesi berhenti).
 */
class AudioFocusControllerTest {

    private class FakeSink : AudioFocusSink {
        var requests = 0
        var abandons = 0
        override fun requestMediaFocus() { requests++ }
        override fun abandonMediaFocus() { abandons++ }
    }

    @Test
    fun requests_once_when_enabled_and_abandons_when_disabled() {
        val sink = FakeSink()
        val controller = AudioFocusController(sink)

        controller.setEnabled(true)
        assertTrue(controller.isActive())
        assertEquals(1, sink.requests)
        assertEquals(0, sink.abandons)

        // Idempoten: enable lagi tidak menambah request.
        controller.setEnabled(true)
        assertEquals(1, sink.requests)

        controller.setEnabled(false)
        assertFalse(controller.isActive())
        assertEquals(1, sink.abandons)
    }

    @Test
    fun release_always_abandons() {
        val sink = FakeSink()
        val controller = AudioFocusController(sink)
        controller.setEnabled(true)
        controller.release()
        assertFalse(controller.isActive())
        assertEquals(1, sink.abandons)
        // Release saat sudah nonaktif tidak menambah abandon.
        controller.release()
        assertEquals(1, sink.abandons)
    }

    @Test
    fun disable_without_enable_does_not_touch_sink() {
        val sink = FakeSink()
        val controller = AudioFocusController(sink)
        controller.setEnabled(false)
        assertEquals(0, sink.requests)
        assertEquals(0, sink.abandons)
    }
}
