import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  test('invoke posts a JSON payload and decodes the JSON answer', () async {
    final h = Harness((r) => ok({'sum': 3}));
    final out = await h.fb.functions.invoke('adder', payload: {'a': 1, 'b': 2});
    expect(out, {'sum': 3});
    expect(h.server.last.method, 'POST');
    expect(h.server.last.path, '/api/v1/functions/adder/invoke');
    expect(h.server.last.map, {'a': 1, 'b': 2});
  });

  test('invoke supports sub-paths, versions and query', () async {
    final h = Harness((r) => Reply(204));
    expect(
      await h.fb.functions.invoke(
        'api',
        path: '/users/a b',
        version: 3,
        query: {'x': 1},
      ),
      isNull,
    );
    expect(h.server.last.path, '/api/v1/functions/api/invoke/users/a%20b');
    expect(h.server.last.query, {'x': '1', 'version': '3'});
    expect(h.server.last.bodyBytes, isEmpty);
  });

  test('invokeRaw returns status, headers and bytes', () async {
    final h = Harness(
      (r) => Reply(200,
          text: 'plain', headers: {'content-type': 'text/plain', 'x-a': 'b'}),
    );
    final r = await h.fb.functions.invokeRaw('f', payload: {'q': 1});
    expect(r.status, 200);
    expect(r.text, 'plain');
    expect(r.headers['x-a'], 'b');
    expect(r.bytes, hasLength(5));
  });

  test('a function failure maps to typed errors', () async {
    final h = Harness((r) => apiError(502, 'function_failed', 'trap'));
    await expectLater(
      h.fb.functions.invoke('f'),
      throwsA(isA<ServerException>()
          .having((e) => e.code, 'code', 'function_failed')),
    );
  });

  test('management routes', () async {
    final h = Harness((r) {
      if (r.method == 'DELETE') return Reply(204);
      if (r.method == 'GET' && !r.path.endsWith('/f')) {
        return ok({
          'items': [
            {'name': 'f'}
          ]
        });
      }
      return ok({'name': 'f'});
    });
    final f = h.fb.functions;
    expect((await f.list()).single['name'], 'f');
    await f.get('f');
    await f.deploy({'name': 'f', 'source': 'serve(() => 1)'});
    expect(h.server.last.map['name'], 'f');
    await f.logs('f', limit: 5);
    expect(h.server.last.query, {'limit': '5'});
    await f.versions('f');
    await f.rollback('f', version: 2);
    expect(h.server.last.map, {'version': 2});
    await f.secrets('f');
    await f.putSecret('f', 'KEY', 'v');
    expect(h.server.last.method, 'PUT');
    expect(h.server.last.path, '/api/v1/functions/f/secrets/KEY');
    await f.deleteSecret('f', 'KEY');
    await f.delete('f');
    expect(h.server.last.path, '/api/v1/functions/f');
  });
}
