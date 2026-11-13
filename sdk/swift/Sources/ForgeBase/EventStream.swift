import Foundation

extension Transport {
    /// A server-sent-event stream that reconnects with exponential backoff (0.5 s doubling to 10 s, reset after a
    /// stream that opened). A refusal (4xx other than 408/429, or an `error` event) ends it with the error.
    /// Cancelling the consuming task closes the connection.
    func eventStream(_ request: @escaping @Sendable () -> Request, reconnect: Bool = true) -> AsyncThrowingStream<SSEEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                var delay: TimeInterval = 0.5
                let sleep = self.retry.sleep
                while !Task.isCancelled {
                    var opened = false
                    do {
                        let (bytes, _) = try await self.openStream(request())
                        opened = true
                        var parser = SSEParser()
                        for try await b in bytes {
                            guard let ev = parser.feed(b) else { continue }
                            if ev.event == "error" {
                                var code = "stream_error"
                                var message = ev.data
                                if let j = try? JSON(data: Data(ev.data.utf8)) {
                                    code = j["code"]?.stringValue ?? code
                                    message = j["message"]?.stringValue ?? message
                                }
                                throw ForgeBaseError.realtime(code, message.isEmpty ? "stream error" : message)
                            }
                            continuation.yield(ev)
                        }
                        if !reconnect {
                            continuation.finish()
                            return
                        }
                    } catch {
                        if Task.isCancelled || error is CancellationError {
                            continuation.finish()
                            return
                        }
                        let e = error as? ForgeBaseError
                        let final = e?.kind == .realtime
                            || (e != nil && (400..<500).contains(e!.status) && e!.status != 429 && e!.status != 408)
                        if final || !reconnect {
                            continuation.finish(throwing: error)
                            return
                        }
                    }
                    if opened { delay = 0.5 }
                    do { try await sleep(delay) } catch {
                        continuation.finish()
                        return
                    }
                    delay = min(delay * 2, 10)
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
