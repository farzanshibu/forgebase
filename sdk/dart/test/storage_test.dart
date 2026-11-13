import 'dart:async';
import 'dart:convert';

import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

import 'helpers.dart';

Map<String, dynamic> obj(String key, {int size = 3, bool latest = true}) => {
      'bucket': 'b',
      'key': key,
      'version_id': 'v1',
      'size': size,
      'content_type': 'text/plain',
      'metadata': {'a': 'b'},
      'is_latest': latest,
      'delete_marker': false,
      'created_by': 'u1',
      'created_at': '2026-01-01T00:00:00Z',
      'checksum_sha256': 'abc',
    };

Map<String, dynamic> uploadJson(
        {int received = 0, String kind = 'multipart'}) =>
    {
      'id': 'up1',
      'bucket': 'b',
      'key': 'big.bin',
      'kind': kind,
      'content_type': 'application/octet-stream',
      'received_bytes': received,
      'parts': <dynamic>[],
      'expires_at': '2026-01-02T00:00:00Z',
    };

void main() {
  test('keyPath encodes segments but keeps slashes', () {
    expect(keyPath('a b/c#d/é.png'), 'a%20b/c%23d/%C3%A9.png');
  });

  test('sha256Hex', () {
    expect(
      sha256Hex(utf8.encode('abc')),
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
  });

  group('buckets', () {
    test('list, create, get, update, delete, usage', () async {
      final h = Harness((r) {
        if (r.method == 'DELETE') return Reply(204);
        if (r.method == 'GET' && r.path == '/api/v1/storage/buckets') {
          return ok({
            'items': [
              {
                'name': 'b',
                'visibility': 'public',
                'versioning': true,
                'owner': 'u',
                'created_at': 'c',
                'updated_at': 'u',
                'allowed_content_types': ['image/*'],
                'max_object_bytes': 100,
              },
            ],
          });
        }
        if (r.path.endsWith('/usage')) return ok({'objects': 1});
        return Reply(
          r.method == 'POST' ? 201 : 200,
          json: {
            'name': 'b',
            'visibility': 'private',
            'versioning': false,
            'owner': 'u',
            'created_at': 'c',
            'updated_at': 'u',
          },
        );
      });
      final s = h.fb.storage;
      final list = await s.listBuckets();
      expect(list.single.name, 'b');
      expect(list.single.versioning, isTrue);
      expect(list.single.allowedContentTypes, ['image/*']);
      expect(list.single.maxObjectBytes, 100);
      await s.createBucket('b', visibility: 'private', versioning: false);
      expect(h.server.last.map,
          {'name': 'b', 'visibility': 'private', 'versioning': false});
      expect((await s.getBucket('b')).visibility, 'private');
      await s.updateBucket('b', visibility: 'public');
      expect(h.server.last.method, 'PATCH');
      expect(h.server.last.map, {'visibility': 'public'});
      expect((await s.bucketUsage('b'))['objects'], 1);
      await s.deleteBucket('b', force: true);
      expect(h.server.last.query, {'force': 'true'});
    });
  });

  group('objects', () {
    test('putObject streams bytes with content type, checksum and metadata',
        () async {
      final h = Harness((r) => Reply(201, json: obj('dir/a b.txt')));
      final bytes = utf8.encode('hey');
      final info = await h.fb.storage.putObject(
        'b',
        'dir/a b.txt',
        bytes,
        contentType: 'text/plain',
        checksumSha256: sha256Hex(bytes),
        metadata: {'owner': 'ada'},
      );
      final r = h.server.last;
      expect(r.method, 'PUT');
      expect(r.path, '/api/v1/storage/buckets/b/objects/dir/a%20b.txt');
      expect(r.body, 'hey');
      expect(r.headers['Content-Type'], 'text/plain');
      expect(r.headers['X-ForgeBase-Checksum-SHA256'], sha256Hex(bytes));
      expect(r.headers['X-ForgeBase-Meta-owner'], 'ada');
      expect(info.size, 3);
      expect(info.metadata, {'a': 'b'});
      expect(info.checksumSha256, 'abc');
      expect(info.isLatest, isTrue);
    });

    test('a checksum mismatch is a validation error', () async {
      final h = Harness((r) => apiError(422, 'checksum_mismatch', 'bad'));
      await expectLater(
        h.fb.storage.putObject('b', 'k', [1], checksumSha256: 'x'),
        throwsA(isA<ValidationException>()),
      );
    });

    test('getObject, ranges and versions', () async {
      final h = Harness((r) => Reply(200, bytes: [1, 2, 3, 4]));
      final s = h.fb.storage;
      expect(await s.getObject('b', 'k', versionId: 'v9'), [1, 2, 3, 4]);
      expect(h.server.last.query, {'version': 'v9'});
      await s.getObjectRange('b', 'k', 2, 5);
      expect(h.server.last.headers['Range'], 'bytes=2-5');
      await s.getObjectRange('b', 'k', 10);
      expect(h.server.last.headers['Range'], 'bytes=10-');
    });

    test('getObjectStream exposes headers and chunks', () async {
      final h = Harness(
        (r) => Reply(
          200,
          stream: Stream.fromIterable([
            [1, 2],
            [3],
          ]),
          headers: {'content-type': 'image/png', 'content-length': '3'},
        ),
      );
      final o = await h.fb.storage.getObjectStream('b', 'k');
      expect(o.contentType, 'image/png');
      expect(o.contentLength, 3);
      expect(await o.bytes(), [1, 2, 3]);
    });

    test('a missing object is a NotFoundException', () async {
      final h = Harness((r) => apiError(404, 'not_found', 'no such object'));
      await expectLater(
        h.fb.storage.getObject('b', 'nope'),
        throwsA(isA<NotFoundException>()),
      );
    });

    test('metadata, list, copy, move, versions, restore, delete', () async {
      final h = Harness((r) {
        if (r.path.endsWith('/objects') && r.method == 'GET') {
          return ok({
            'objects': [obj('a')],
            'prefixes': ['dir/'],
            'next_cursor': 'c2',
          });
        }
        if (r.path.contains('/versions/')) {
          return ok({
            'items': [obj('a'), obj('a', latest: false)],
          });
        }
        if (r.method == 'DELETE') {
          return ok({'delete_marker': true, 'version_id': 'v2'});
        }
        return ok(obj('a'));
      });
      final s = h.fb.storage;
      expect((await s.metadata('b', 'a')).key, 'a');
      expect(h.server.last.path, '/api/v1/storage/buckets/b/meta/a');
      final page =
          await s.listObjects('b', prefix: 'dir/', delimiter: '/', limit: 10);
      expect(page.objects.single.key, 'a');
      expect(page.prefixes, ['dir/']);
      expect(page.nextCursor, 'c2');
      expect(h.server.last.query,
          {'prefix': 'dir/', 'delimiter': '/', 'limit': '10'});
      await s.copyObject('b', 'a', 'c', destBucket: 'b2');
      expect(h.server.last.map,
          {'key': 'a', 'dest_key': 'c', 'dest_bucket': 'b2'});
      await s.moveObject('b', 'a', 'd');
      expect(h.server.last.path, '/api/v1/storage/buckets/b/move');
      expect(await s.listVersions('b', 'a'), hasLength(2));
      await s.restoreVersion('b', 'a', 'v1');
      expect(h.server.last.map, {'key': 'a', 'version_id': 'v1'});
      final del = await s.deleteObject('b', 'a');
      expect(del['delete_marker'], isTrue);
      await s.deleteObject('b', 'a', versionId: 'v1');
      expect(h.server.last.query, {'version': 'v1'});
    });
  });

  group('signed URLs', () {
    test('sign requests the right body and downloads without credentials',
        () async {
      final h = Harness((r) {
        if (r.path.endsWith('/sign')) {
          return ok({
            'url': '/api/v1/storage/signed/b/dir/a.txt?exp=1&sig=abc',
            'method': 'GET',
            'expires_at': '2026-01-01T00:15:00Z',
          });
        }
        return Reply(200, bytes: [9, 9]);
      }, apiKey: 'fb_k');
      final signed = await h.fb.storage.sign(
        'b',
        'dir/a.txt',
        expiresIn: const Duration(minutes: 5),
        downloadAs: 'x.txt',
      );
      expect(h.server.last.map, {
        'key': 'dir/a.txt',
        'method': 'GET',
        'expires_in': 300,
        'download_as': 'x.txt',
      });
      expect(signed.expiresAt, '2026-01-01T00:15:00Z');
      expect(
        h.fb.storage.signedUrlFor(signed),
        '$base/api/v1/storage/signed/b/dir/a.txt?exp=1&sig=abc',
      );
      expect(await h.fb.storage.downloadSigned(signed), [9, 9]);
      final r = h.server.last;
      expect(r.path, '/api/v1/storage/signed/b/dir/a.txt');
      expect(r.query, {'exp': '1', 'sig': 'abc'});
      expect(r.headers.containsKey('X-API-Key'), isFalse);
    });

    test('uploadSigned PUTs the bytes anonymously', () async {
      final h = Harness((r) => Reply(201, json: obj('up.txt')), apiKey: 'fb_k');
      const signed = SignedUrl(
        url: '/api/v1/storage/signed/b/up.txt?sig=1',
        method: 'PUT',
        expiresAt: 'x',
      );
      final info = await h.fb.storage
          .uploadSigned(signed, [1, 2], contentType: 'text/plain');
      expect(h.server.last.method, 'PUT');
      expect(h.server.last.headers['Content-Type'], 'text/plain');
      expect(h.server.last.headers.containsKey('X-API-Key'), isFalse);
      expect(info.key, 'up.txt');
    });

    test('foreign URLs are rejected', () async {
      final h = Harness((r) => ok({}));
      await expectLater(
        h.fb.storage.downloadSigned(
          const SignedUrl(
              url: 'https://evil.example/x', method: 'GET', expiresAt: 'x'),
        ),
        throwsArgumentError,
      );
      expect(h.server.log, isEmpty);
    });
  });

  group('multipart', () {
    test('uploads parts in order, completes with their checksums', () async {
      final h = Harness((r) {
        if (r.path == '/api/v1/storage/buckets/b/uploads') {
          return Reply(201, json: uploadJson());
        }
        if (r.path.contains('/parts/')) {
          final n = int.parse(r.path.split('/').last);
          return ok({
            'part_number': n,
            'size': r.bodyBytes.length,
            'checksum_sha256': 'h$n'
          });
        }
        return Reply(201, json: obj('big.bin', size: 10));
      });
      final progress = <int>[];
      final info = await h.fb.storage.uploadMultipart(
        'b',
        'big.bin',
        List<int>.generate(10, (i) => i),
        partSize: 4,
        contentType: 'application/x-test',
        onProgress: (sent, total) => progress.add(sent),
      );
      expect(info.size, 10);
      expect(h.server.log.first.map, {
        'key': 'big.bin',
        'kind': 'multipart',
        'content_type': 'application/x-test',
        'size': 10,
      });
      final parts =
          h.server.log.where((r) => r.path.contains('/parts/')).toList();
      expect(parts.map((r) => r.bodyBytes.length), [4, 4, 2]);
      expect(parts.map((r) => r.method).toSet(), {'PUT'});
      expect(progress, [4, 8, 10]);
      expect(h.server.last.path, '/api/v1/storage/uploads/up1/complete');
      expect(h.server.last.map['parts'], [
        {'part_number': 1, 'checksum_sha256': 'h1'},
        {'part_number': 2, 'checksum_sha256': 'h2'},
        {'part_number': 3, 'checksum_sha256': 'h3'},
      ]);
    });

    test('aborts the session when a part fails', () async {
      final h = Harness((r) {
        if (r.path.endsWith('/uploads')) return Reply(201, json: uploadJson());
        if (r.path.endsWith('/parts/2')) return apiError(400, 'bad_part', 'x');
        if (r.method == 'DELETE') return Reply(204);
        return ok({'part_number': 1, 'size': 1, 'checksum_sha256': 'h'});
      });
      await expectLater(
        h.fb.storage.uploadMultipart('b', 'big.bin', [1, 2, 3, 4], partSize: 2),
        throwsA(isA<ValidationException>()),
      );
      expect(h.server.last.method, 'DELETE');
      expect(h.server.last.path, '/api/v1/storage/uploads/up1');
    });
  });

  group('resumable', () {
    test('appends chunks at the right offsets until the object appears',
        () async {
      final h = Harness((r) {
        if (r.path.endsWith('/uploads')) {
          return Reply(201, json: uploadJson(kind: 'resumable'));
        }
        if (r.method == 'PATCH') {
          final off = int.parse(r.headers['Upload-Offset']!);
          if (off + r.bodyBytes.length >= 6) {
            return Reply(201, json: obj('big.bin', size: 6));
          }
          return ok(uploadJson(
              received: off + r.bodyBytes.length, kind: 'resumable'));
        }
        return ok({});
      });
      final info = await h.fb.storage.uploadResumable(
        'b',
        'big.bin',
        [1, 2, 3, 4, 5, 6],
        chunkSize: 4,
      );
      expect(info.size, 6);
      final patches = h.server.log.where((r) => r.method == 'PATCH').toList();
      expect(patches.map((r) => r.headers['Upload-Offset']), ['0', '4']);
      expect(patches.first.headers['Content-Type'],
          'application/offset+octet-stream');
      expect(h.server.log.first.map['kind'], 'resumable');
    });

    test('resumes from the server offset', () async {
      final h = Harness((r) {
        if (r.method == 'GET') {
          return ok(uploadJson(received: 4, kind: 'resumable'));
        }
        return Reply(201, json: obj('big.bin', size: 6));
      });
      await h.fb.storage
          .uploadResumable('b', 'big.bin', [1, 2, 3, 4, 5, 6], resumeId: 'up1');
      final patches = h.server.log.where((r) => r.method == 'PATCH').toList();
      expect(patches.single.headers['Upload-Offset'], '4');
      expect(patches.single.bodyBytes, [5, 6]);
    });

    test('getUpload exposes where to continue', () async {
      final h = Harness((r) => ok(uploadJson(received: 7)));
      expect((await h.fb.storage.getUpload('up1')).receivedBytes, 7);
    });
  });
}
