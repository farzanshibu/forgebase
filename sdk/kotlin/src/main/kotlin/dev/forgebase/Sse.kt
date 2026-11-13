package dev.forgebase

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.launch
import okhttp3.Response
import java.io.IOException

internal data class SseEvent(val event: String, val data: String, val id: String?)

/** Parses a `text/event-stream` body into events; closing the response (or cancelling the collector) stops it. */
internal fun sseEvents(res: Response): Flow<SseEvent> = flow {
    coroutineScope {
        // Closing the response unblocks a read that is waiting for the next event when the collector is cancelled.
        val watcher = launch {
            try {
                awaitCancellation()
            } finally {
                runCatching { res.close() }
            }
        }
        try {
            val source = res.body?.source() ?: throw NetworkException("the event stream has no body")
            var event = "message"
            val data = StringBuilder()
            var id: String? = null
            var hasData = false
            while (true) {
                currentCoroutineContext().ensureActive()
                val line = try {
                    source.readUtf8Line()
                } catch (e: IOException) {
                    currentCoroutineContext().ensureActive()
                    throw NetworkException(e.message ?: "event stream interrupted", e)
                } ?: break
                when {
                    line.isEmpty() -> {
                        if (hasData || event != "message") emit(SseEvent(event, data.toString(), id))
                        event = "message"
                        data.setLength(0)
                        hasData = false
                        id = null
                    }
                    line.startsWith(":") -> Unit
                    line.startsWith("event:") -> event = line.substring(6).trim()
                    line.startsWith("data:") -> {
                        if (hasData) data.append('\n')
                        data.append(line.substring(5).removePrefix(" "))
                        hasData = true
                    }
                    line.startsWith("id:") -> id = line.substring(3).trim()
                }
            }
        } finally {
            watcher.cancel()
            runCatching { res.close() }
        }
    }
}.flowOn(Dispatchers.IO)
