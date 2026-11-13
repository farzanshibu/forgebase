import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

void main() {
  Stream<List<int>> chunks(List<String> parts) =>
      Stream.fromIterable(parts.map(utf8.encode));

  test('parses events split across chunks, CRLF and comments', () async {
    final events = await parseSse(
      chunks([
        ': hello\r\n\r\nevent: message\r\nda',
        'ta: {"a":1}\r\nid: 5\r\n\r\n',
        'data: line1\ndata: line2\n\nevent: ping\n\n',
      ]),
    ).toList();
    expect(events.map((e) => e.event), ['message', 'message', 'ping']);
    expect(events[0].data, '{"a":1}');
    expect(events[0].id, '5');
    expect(events[1].data, 'line1\nline2');
    expect(events[2].data, isEmpty);
  });

  test('handles multi-byte characters split across chunks', () async {
    final bytes = utf8.encode('data: héllo\n\n');
    final events = await parseSse(
      Stream.fromIterable([bytes.sublist(0, 8), bytes.sublist(8)]),
    ).toList();
    expect(events.single.data, 'héllo');
  });

  test('an unterminated trailing event is dropped', () async {
    final events =
        await parseSse(chunks(['data: x\n\ndata: partial'])).toList();
    expect(events.map((e) => e.data), ['x']);
  });

  test('cancel reaches the source immediately', () async {
    final src = StreamController<List<int>>();
    final sub = parseSse(src.stream).listen((_) {});
    await Future<void>.delayed(Duration.zero);
    expect(src.hasListener, isTrue);
    await sub.cancel();
    expect(src.hasListener, isFalse);
  });
}
