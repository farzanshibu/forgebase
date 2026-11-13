import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  test('kvEncode / kvDecode handle text and bytes', () {
    expect(kvEncode('hi'), 'hi');
    expect(kvEncode([0, 255]), {'base64': 'AP8='});
    expect(kvDecode('hi'), 'hi');
    expect(kvDecode({'base64': 'AP8='}), isA<Uint8List>());
    expect(kvDecode({'base64': 'AP8='}), [0, 255]);
    expect(kvDecode(5), 5);
    expect(() => kvEncode(5), throwsArgumentError);
  });

  test('set sends value, TTL and conditions', () async {
    final h = Harness((r) => ok({'ok': true}));
    final kv = h.fb.kv('cache/ns');
    await kv.set('a b', 'v', ttl: const Duration(seconds: 2), nx: true);
    expect(h.server.last.method, 'PUT');
    expect(h.server.last.path, '/api/v1/kv/cache%2Fns/strings/a%20b');
    expect(h.server.last.map, {'value': 'v', 'ttl_ms': 2000, 'nx': true});
    await kv.set('b', [1, 2], keepTtl: true, xx: true);
    expect(h.server.last.map, {
      'value': {'base64': 'AQI='},
      'xx': true,
      'keepttl': true,
    });
  });

  test('get, getString and getValue map missing keys to null', () async {
    final h = Harness((r) {
      if (r.path.endsWith('/missing')) {
        return apiError(404, 'key_not_found', 'no key');
      }
      if (r.path.endsWith('/bin')) {
        return ok({
          'key': 'bin',
          'type': 'string',
          'size': 2,
          'value': {'base64': 'AP8='}
        });
      }
      return ok({
        'key': 'k',
        'type': 'string',
        'size': 2,
        'ttl_ms': 1500,
        'truncated': false,
        'value': 'hi',
      });
    });
    final kv = h.fb.kv('ns');
    final k = await kv.get('k');
    expect(k.type, 'string');
    expect(k.ttl, const Duration(milliseconds: 1500));
    expect(k.value, 'hi');
    expect(await kv.getString('k'), 'hi');
    expect(await kv.getString('missing'), isNull);
    expect(await kv.getValue('missing'), isNull);
    expect(await kv.getValue('bin'), [0, 255]);
    expect(await kv.getString('bin'), isNull, reason: 'not UTF-8');
    await expectLater(kv.get('missing'), throwsA(isA<NotFoundException>()));
  });

  test('incr, expire, persist, delete, flush', () async {
    final h = Harness((r) => ok({'value': 3}));
    final kv = h.fb.kv('ns');
    await kv.incr('n', by: 2);
    expect(h.server.last.path, '/api/v1/kv/ns/strings/n/incr');
    expect(h.server.last.map, {'by': 2});
    await kv.incr('n');
    expect(h.server.last.map, <String, dynamic>{});
    await kv.expire('n', const Duration(minutes: 1));
    expect(h.server.last.method, 'PUT');
    expect(h.server.last.map, {'ttl_ms': 60000});
    await kv.persist('n');
    expect(h.server.last.method, 'DELETE');
    expect(h.server.last.path, '/api/v1/kv/ns/keys/n/ttl');
    await kv.delete('n');
    expect(h.server.last.path, '/api/v1/kv/ns/keys/n');
    await kv.flush();
    expect(h.server.last.path, '/api/v1/kv/ns');
    expect(h.server.last.method, 'DELETE');
  });

  test('keys follows the scan cursor to the end', () async {
    final h = Harness((r) {
      final c = r.query['cursor'];
      return c == '0'
          ? ok({
              'keys': ['a', 'b'],
              'cursor': '17'
            })
          : ok({
              'keys': ['c'],
              'cursor': '0'
            });
    });
    final all = await h.fb.kv('ns').keys(match: 'x*', count: 2).toList();
    expect(all, ['a', 'b', 'c']);
    expect(
        h.server.log.first.query, {'cursor': '0', 'match': 'x*', 'count': '2'});
    expect(h.server.log.last.query['cursor'], '17');
  });

  test('mget / mset', () async {
    final h = Harness((r) => ok({'values': <dynamic>[]}));
    final kv = h.fb.kv('ns');
    await kv.mget(['a', 'b']);
    expect(h.server.last.map, {
      'keys': ['a', 'b']
    });
    await kv.mset({
      'a': '1',
      'b': [2]
    });
    expect(h.server.last.map, {
      'entries': [
        {'key': 'a', 'value': '1'},
        {
          'key': 'b',
          'value': {'base64': 'Ag=='}
        },
      ],
    });
  });

  test('hashes, lists, sets, sorted sets, streams', () async {
    final h = Harness((r) => ok({}));
    final kv = h.fb.kv('ns');
    await kv.hashSet('h', {'f': 'v'});
    expect(h.server.last.map, {
      'fields': {'f': 'v'}
    });
    await kv.hashIncr('h', 'n', by: 5);
    expect(h.server.last.map, {'field': 'n', 'by': 5});
    await kv.hashDelete('h', ['f']);
    expect(h.server.last.map, {
      'fields': ['f']
    });
    await kv.listPush('l', ['a', 'b'], left: true);
    expect(h.server.last.path, '/api/v1/kv/ns/lists/l/push');
    expect(h.server.last.map, {
      'side': 'left',
      'values': ['a', 'b']
    });
    await kv.listPop('l', count: 2);
    expect(h.server.last.map, {'side': 'left', 'count': 2});
    await kv.setAdd('s', ['x']);
    expect(h.server.last.path, '/api/v1/kv/ns/sets/s/add');
    await kv.setRemove('s', ['x']);
    await kv.zsetAdd('z', {'a': 1.5});
    expect(h.server.last.map, {
      'members': [
        {'member': 'a', 'score': 1.5},
      ],
    });
    await kv.zsetRemove('z', ['a']);
    expect(h.server.last.path, '/api/v1/kv/ns/zsets/z/remove');
    await kv.streamAdd('st', {'k': 'v'}, maxlen: 100);
    expect(h.server.last.map, {
      'fields': {'k': 'v'},
      'maxlen': 100
    });
    await kv.streamRange('st', start: '-', end: '+', count: 3, reverse: true);
    expect(h.server.last.query,
        {'start': '-', 'end': '+', 'count': '3', 'rev': 'true'});
    await kv.streamRead('st', after: r'$');
    expect(h.server.last.query, {'after': r'$'});
  });

  test('exec and batch stringify arguments', () async {
    final h = Harness((r) => ok({'reply': 1}));
    final kv = h.fb.kv('ns');
    await kv.exec(['INCRBY', 'hits', 5]);
    expect(h.server.last.map, {
      'command': ['INCRBY', 'hits', '5']
    });
    await kv.batch([
      ['SET', 'a', '1'],
      ['INCR', 'a'],
    ]);
    expect(h.server.last.map, {
      'commands': [
        ['SET', 'a', '1'],
        ['INCR', 'a'],
      ],
    });
  });

  test('publish, info, namespaces, channels', () async {
    final h = Harness((r) => ok({'ok': 1}));
    final kv = h.fb.kv('ns');
    await kv.publish('news', 'hello');
    expect(h.server.last.path, '/api/v1/kv/ns/publish/news');
    expect(h.server.last.map, {'message': 'hello'});
    await kv.info();
    expect(h.server.last.path, '/api/v1/kv/_info');
    await kv.namespaces();
    expect(h.server.last.path, '/api/v1/kv/_namespaces');
    await kv.channels();
    expect(h.server.last.path, '/api/v1/kv/ns/channels');
  });

  test('subscribe streams messages and cancels the connection', () async {
    final body = StreamController<List<int>>();
    final h = Harness((r) {
      expect(r.path, '/api/v1/kv/ns/subscribe/news');
      return Reply(200,
          stream: body.stream, headers: {'content-type': 'text/event-stream'});
    });
    final got = <KvMessage>[];
    final sub = h.fb.kv('ns').subscribe('news').listen(got.add);
    await waitFor(() => body.hasListener);
    body.add(utf8.encode(
      'event: message\ndata: ${jsonEncode({
            'channel': 'news',
            'message': 'a'
          })}\n\n'
      'event: lagged\ndata: {"missed":2}\n\n'
      'event: message\ndata: ${jsonEncode({
            'channel': 'news',
            'message': {'base64': 'AP8='}
          })}\n\n',
    ));
    await waitFor(() => got.length == 2);
    expect(got[0].message, 'a');
    expect(got[1].message, [0, 255]);
    await sub.cancel();
    expect(body.hasListener, isFalse);
  });

  test('subscribe surfaces connection errors on the stream', () async {
    final h = Harness((r) => apiError(403, 'forbidden', 'no'));
    await expectLater(
      h.fb.kv('ns').subscribe('news'),
      emitsError(isA<PermissionException>()),
    );
  });
}
