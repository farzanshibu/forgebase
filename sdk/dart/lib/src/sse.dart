import 'dart:async';
import 'dart:convert';

/// One server-sent event.
class SseEvent {
  /// Creates an event.
  const SseEvent({required this.event, required this.data, this.id});

  /// The `event:` name (`message` by default).
  final String event;

  /// The `data:` lines joined by newlines.
  final String data;

  /// The `id:` field.
  final String? id;
}

/// Parses a `text/event-stream` body into events. Tolerates events split
/// across chunks and CRLF line ends; comment lines are skipped.
Stream<SseEvent> parseSse(Stream<List<int>> body) async* {
  final decoded = body.transform(utf8.decoder);
  var buf = '';
  await for (final chunk in decoded) {
    buf = (buf + chunk).replaceAll('\r\n', '\n');
    int i;
    while ((i = buf.indexOf('\n\n')) >= 0) {
      final block = buf.substring(0, i);
      buf = buf.substring(i + 2);
      var event = 'message';
      final data = <String>[];
      String? id;
      for (final line in block.split('\n')) {
        if (line.startsWith(':')) continue;
        final c = line.indexOf(':');
        final field = c < 0 ? line : line.substring(0, c);
        var val = c < 0 ? '' : line.substring(c + 1);
        if (val.startsWith(' ')) val = val.substring(1);
        switch (field) {
          case 'event':
            event = val.trim();
          case 'data':
            data.add(val);
          case 'id':
            id = val.trim();
        }
      }
      if (data.isNotEmpty || event != 'message') {
        yield SseEvent(event: event, data: data.join('\n'), id: id);
      }
    }
  }
}
