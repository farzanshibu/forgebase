import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

Map<String, dynamic> page(
  List<Map<String, dynamic>> items, {
  int? pageNo,
  int? totalPages,
  String? next,
}) =>
    {
      'items': items,
      'per_page': 2,
      if (pageNo != null) 'page': pageNo,
      if (totalPages != null) 'total_pages': totalPages,
      if (pageNo != null) 'total_items': 5,
      if (next != null) 'next_cursor': next,
    };

void main() {
  test('list sends a plain GET with filter, sort, expand and fields', () async {
    final h = Harness((r) => ok(page([
          {'id': 'a'}
        ], pageNo: 1, totalPages: 1)));
    final res = await h.fb.collection('posts').list(
          const ListOptions(
            filter: 'status = "live"',
            sort: '-created,name',
            expand: ['author', 'tags'],
            fields: ['id', 'title'],
            search: 'dart',
            page: 2,
            perPage: 10,
            skipTotal: true,
          ),
        );
    final r = h.server.last;
    expect(r.method, 'GET');
    expect(r.path, '/api/v1/collections/posts/records');
    expect(r.query, {
      'filter': 'status = "live"',
      'sort': '-created,name',
      'expand': 'author,tags',
      'fields': 'id,title',
      'search': 'dart',
      'page': '2',
      'per_page': '10',
      'skip_total': 'true',
    });
    expect(res.items.single['id'], 'a');
    expect(res.page, 1);
    expect(res.totalItems, 5);
    expect(res.nextCursor, isNull);
  });

  test('list with params uses the typed query endpoint', () async {
    final h = Harness((r) => ok(page([])));
    await h.fb.collection('posts').list(
          const ListOptions(
            filter: 'price > {:min}',
            params: {'min': 5},
            expand: ['author'],
            perPage: 3,
            consistency: Consistency.strong,
          ),
        );
    final r = h.server.last;
    expect(r.method, 'POST');
    expect(r.path, '/api/v1/collections/posts/records/query');
    expect(r.map, {
      'filter': 'price > {:min}',
      'per_page': 3,
      'params': {'min': 5},
      'expand': ['author'],
    });
    expect(r.headers['X-ForgeBase-Consistency'], 'strong');
  });

  test('pages follows the cursor', () async {
    final seen = <String?>[];
    final h = Harness((r) {
      seen.add(r.query['cursor']);
      return switch (r.query['cursor']) {
        null => ok(page([
            {'id': '1'},
            {'id': '2'}
          ], next: 'c1')),
        'c1' => ok(page([
            {'id': '3'}
          ], next: 'c2')),
        _ => ok(page([
            {'id': '4'}
          ])),
      };
    });
    final ids = await h.fb
        .collection('posts')
        .iterate(const ListOptions(perPage: 2))
        .map((r) => r['id'])
        .toList();
    expect(ids, ['1', '2', '3', '4']);
    expect(seen, [null, 'c1', 'c2']);
  });

  test('pages falls back to page numbers', () async {
    final h = Harness((r) {
      final p = int.parse(r.query['page'] ?? '1');
      return ok(page([
        {'id': 'p$p'}
      ], pageNo: p, totalPages: 3));
    });
    final all = await h.fb.collection('posts').all();
    expect(all.map((r) => r['id']), ['p1', 'p2', 'p3']);
    expect(h.server.log.map((r) => r.query['page']), ['1', '2', '3']);
  });

  test('first returns one record or null', () async {
    final h = Harness((r) => ok(page([])));
    expect(await h.fb.collection('posts').first('slug = "x"'), isNull);
    expect(h.server.last.query['per_page'], '1');
    expect(h.server.last.query['skip_total'], 'true');
    final h2 = Harness((r) => ok(page([
          {'id': 'z'}
        ])));
    expect((await h2.fb.collection('posts').first('x'))!['id'], 'z');
  });

  test('get, create, update, replace, delete', () async {
    final h = Harness((r) {
      if (r.method == 'DELETE') return Reply(204);
      return ok({'id': 'r 1', '_version': 2});
    });
    final posts = h.fb.collection('posts');
    await posts.get('r 1', expand: ['a'], fields: ['id']);
    expect(h.server.last.path, '/api/v1/collections/posts/records/r%201');
    expect(h.server.last.query, {'expand': 'a', 'fields': 'id'});

    await posts.create({'title': 'T'});
    expect(h.server.last.method, 'POST');
    expect(h.server.last.map, {'title': 'T'});

    await posts.update('r1', {'title': 'U'});
    expect(h.server.last.method, 'PATCH');
    expect(h.server.last.headers.containsKey('If-Match'), isFalse);

    await posts.replace('r1', {'title': 'V'}, version: 3);
    expect(h.server.last.method, 'PUT');
    expect(h.server.last.headers['If-Match'], '"3"');

    await posts.delete('r1');
    expect(h.server.last.method, 'DELETE');
  });

  test('optimistic update sends If-Match and a stale write is a conflict',
      () async {
    final h = Harness((r) {
      if (r.headers['If-Match'] == '"1"') {
        return apiError(412, 'version_mismatch', 'stale');
      }
      return ok({'id': 'r1', '_version': 2});
    });
    final posts = h.fb.collection('posts');
    await expectLater(
      posts.update('r1', {'a': 1}, version: 1),
      throwsA(isA<ConflictException>()),
    );
    final ok2 = await posts.update('r1', {'a': 1}, version: 2);
    expect(ok2['_version'], 2);
  });

  test('count, export and import', () async {
    final h = Harness((r) {
      if (r.path.endsWith('/count')) return ok({'count': 7});
      if (r.path.endsWith('/export')) return Reply(200, text: 'id\n1\n');
      return ok({'imported': 2});
    });
    final posts = h.fb.collection('posts');
    expect(await posts.count('x = 1'), 7);
    expect(h.server.last.query, {'filter': 'x = 1'});
    expect(await posts.export(TransferFormat.csv), 'id\n1\n');
    expect(h.server.last.query, {'format': 'csv'});
    final r = await posts.import(TransferFormat.ndjson, '{"a":"é"}\n');
    expect(r['imported'], 2);
    expect(h.server.last.body, '{"a":"é"}\n');
    expect(h.server.last.headers['Content-Type'], 'application/octet-stream');
    expect(h.server.last.query, {'format': 'ndjson'});
  });

  test('decode maps rows to a custom type', () async {
    final h = Harness((r) => ok(page([
          {'id': 'a', 'title': 'T'}
        ])));
    final posts =
        h.fb.collectionOf<String>('posts', decode: (r) => '${r['title']}');
    expect((await posts.list()).items, ['T']);
  });

  test('collection names are percent-encoded', () async {
    final h = Harness((r) => ok(page([])));
    await h.fb.collection('a/b').list();
    expect(h.server.last.path, '/api/v1/collections/a%2Fb/records');
  });

  test('collection definitions', () async {
    final h = Harness((r) {
      if (r.method == 'GET' && r.path == '/api/v1/collections') {
        return ok({
          'items': [
            {'name': 'posts'}
          ]
        });
      }
      if (r.method == 'DELETE') return Reply(204);
      return ok({'name': 'posts'});
    });
    expect((await h.fb.collections.list()).single['name'], 'posts');
    await h.fb.collections.create({'name': 'posts'});
    await h.fb.collections.update('posts', {'name': 'posts'});
    expect(h.server.last.method, 'PUT');
    await h.fb.collections.delete('posts');
    expect(h.server.last.path, '/api/v1/collections/posts');
  });
}
