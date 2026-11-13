import 'dart:async';
import 'dart:typed_data';

import 'package:crypto/crypto.dart' as crypto;
import 'package:http/http.dart' as http;

import 'errors.dart';
import 'http.dart';

String _enc(String s) => Uri.encodeComponent(s);

/// Percent-encodes each segment of an object key, keeping the slashes.
String keyPath(String key) => key.split('/').map(_enc).join('/');

/// Hex SHA-256 of [bytes], for the `checksum` options of uploads.
String sha256Hex(List<int> bytes) => crypto.sha256.convert(bytes).toString();

/// A bucket.
class Bucket {
  /// Creates a bucket.
  const Bucket({
    required this.name,
    required this.visibility,
    required this.versioning,
    required this.owner,
    required this.createdAt,
    required this.updatedAt,
    this.allowedContentTypes,
    this.maxObjectBytes,
    this.tenant,
  });

  /// Reads the server's JSON.
  factory Bucket.fromJson(Json j) => Bucket(
        name: j['name'] as String,
        visibility: j['visibility'] as String? ?? 'private',
        versioning: j['versioning'] == true,
        owner: j['owner'] as String? ?? '',
        createdAt: j['created_at'] as String? ?? '',
        updatedAt: j['updated_at'] as String? ?? '',
        allowedContentTypes: (j['allowed_content_types'] as List<dynamic>?)
            ?.map((e) => '$e')
            .toList(),
        maxObjectBytes: (j['max_object_bytes'] as num?)?.toInt(),
        tenant: j['tenant'] as String?,
      );

  /// Name.
  final String name;

  /// `private`, `authenticated` or `public`.
  final String visibility;

  /// Whether overwrites and deletes keep earlier versions.
  final bool versioning;

  /// Principal key of the creator.
  final String owner;

  /// RFC 3339 creation time.
  final String createdAt;

  /// RFC 3339 time of the last change.
  final String updatedAt;

  /// Allowed content types (`image/*` patterns allowed); `null` allows all.
  final List<String>? allowedContentTypes;

  /// Largest object in bytes; `null`: the server limit.
  final int? maxObjectBytes;

  /// Owning tenant; `null` for an instance-wide bucket.
  final String? tenant;
}

/// An object version.
class ObjectInfo {
  /// Creates an object.
  const ObjectInfo({
    required this.bucket,
    required this.key,
    required this.versionId,
    required this.size,
    required this.contentType,
    required this.metadata,
    required this.isLatest,
    required this.deleteMarker,
    required this.createdBy,
    required this.createdAt,
    this.checksumSha256,
  });

  /// Reads the server's JSON.
  factory ObjectInfo.fromJson(Json j) => ObjectInfo(
        bucket: j['bucket'] as String? ?? '',
        key: j['key'] as String? ?? '',
        versionId: j['version_id'] as String? ?? '',
        size: (j['size'] as num?)?.toInt() ?? 0,
        contentType: j['content_type'] as String? ?? '',
        metadata: {
          for (final e in ((j['metadata'] as Map<String, dynamic>?) ??
                  const <String, dynamic>{})
              .entries)
            e.key: '${e.value}',
        },
        isLatest: j['is_latest'] == true,
        deleteMarker: j['delete_marker'] == true,
        createdBy: j['created_by'] as String? ?? '',
        createdAt: j['created_at'] as String? ?? '',
        checksumSha256: j['checksum_sha256'] as String?,
      );

  /// Bucket.
  final String bucket;

  /// Key.
  final String key;

  /// Version id.
  final String versionId;

  /// Size in bytes.
  final int size;

  /// Content type.
  final String contentType;

  /// User metadata.
  final Map<String, String> metadata;

  /// Whether this is the current version.
  final bool isLatest;

  /// Whether this version is a delete marker.
  final bool deleteMarker;

  /// Principal key of the uploader.
  final String createdBy;

  /// RFC 3339 upload time.
  final String createdAt;

  /// SHA-256 of the content (hex); also the ETag.
  final String? checksumSha256;
}

/// A page of an object listing.
class ObjectPage {
  /// Creates a page.
  const ObjectPage({
    required this.objects,
    required this.prefixes,
    this.nextCursor,
  });

  /// Reads the server's JSON.
  factory ObjectPage.fromJson(Json j) => ObjectPage(
        objects: [
          for (final o in (j['objects'] as List<dynamic>? ?? const []))
            ObjectInfo.fromJson(o as Json),
        ],
        prefixes: [
          for (final p in (j['prefixes'] as List<dynamic>? ?? const [])) '$p',
        ],
        nextCursor: j['next_cursor'] as String?,
      );

  /// Objects (current versions only), in key order.
  final List<ObjectInfo> objects;

  /// "Directories" when a delimiter was given.
  final List<String> prefixes;

  /// Pass as `cursor` for the next page; `null` at the end.
  final String? nextCursor;
}

/// A signed URL: usable without credentials until [expiresAt].
class SignedUrl {
  /// Creates a signed URL.
  const SignedUrl({
    required this.url,
    required this.method,
    required this.expiresAt,
  });

  /// Reads the server's JSON.
  factory SignedUrl.fromJson(Json j) => SignedUrl(
        url: j['url'] as String,
        method: j['method'] as String? ?? 'GET',
        expiresAt: j['expires_at'] as String? ?? '',
      );

  /// Path and query relative to the server root.
  final String url;

  /// `GET` or `PUT`.
  final String method;

  /// RFC 3339 expiry.
  final String expiresAt;
}

/// One received part of a multipart upload.
class PartInfo {
  /// Creates a part.
  const PartInfo({
    required this.partNumber,
    required this.size,
    required this.checksumSha256,
  });

  /// Reads the server's JSON.
  factory PartInfo.fromJson(Json j) => PartInfo(
        partNumber: (j['part_number'] as num).toInt(),
        size: (j['size'] as num?)?.toInt() ?? 0,
        checksumSha256: j['checksum_sha256'] as String? ?? '',
      );

  /// Part number (1-10000).
  final int partNumber;

  /// Size in bytes.
  final int size;

  /// SHA-256 of the part (hex).
  final String checksumSha256;
}

/// A multipart or resumable upload session.
class Upload {
  /// Creates a session.
  const Upload({
    required this.id,
    required this.bucket,
    required this.key,
    required this.kind,
    required this.contentType,
    required this.receivedBytes,
    required this.parts,
    required this.expiresAt,
    this.declaredSize,
    this.declaredSha256,
  });

  /// Reads the server's JSON.
  factory Upload.fromJson(Json j) => Upload(
        id: j['id'] as String,
        bucket: j['bucket'] as String? ?? '',
        key: j['key'] as String? ?? '',
        kind: j['kind'] as String? ?? 'multipart',
        contentType: j['content_type'] as String? ?? '',
        receivedBytes: (j['received_bytes'] as num?)?.toInt() ?? 0,
        parts: [
          for (final p in (j['parts'] as List<dynamic>? ?? const []))
            PartInfo.fromJson(p as Json),
        ],
        expiresAt: j['expires_at'] as String? ?? '',
        declaredSize: (j['declared_size'] as num?)?.toInt(),
        declaredSha256: j['declared_sha256'] as String?,
      );

  /// Session id.
  final String id;

  /// Bucket.
  final String bucket;

  /// Target key.
  final String key;

  /// `multipart` (numbered parts, any order) or `resumable` (sequential
  /// offsets).
  final String kind;

  /// Content type of the finished object.
  final String contentType;

  /// Bytes received so far: where a resumable client continues.
  final int receivedBytes;

  /// Received parts.
  final List<PartInfo> parts;

  /// RFC 3339 expiry.
  final String expiresAt;

  /// Declared total size.
  final int? declaredSize;

  /// Declared SHA-256 of the whole object.
  final String? declaredSha256;
}

/// A download in progress: read it in chunks instead of holding the whole
/// object.
class ObjectStream {
  /// Wraps a streamed response.
  ObjectStream(this._res);

  final http.StreamedResponse _res;

  /// Content length when the server sent one.
  int? get contentLength => _res.contentLength;

  /// `Content-Type` of the object.
  String? get contentType => _res.headers['content-type'];

  /// The body.
  Stream<List<int>> get stream => _res.stream;

  /// Reads the rest of the body.
  Future<Uint8List> bytes() => _res.stream.toBytes();
}

/// Object storage: `client.storage`.
class StorageApi {
  /// Creates the API.
  StorageApi(this._t);

  final Transport _t;

  String _obj(String bucket, String key) =>
      '/api/v1/storage/buckets/${_enc(bucket)}/objects/${keyPath(key)}';

  // ---- buckets ----

  /// Every bucket the caller may see.
  Future<List<Bucket>> listBuckets() async {
    final r = await _t.requestJson('/api/v1/storage/buckets');
    return [
      for (final b in (r['items'] as List<dynamic>? ?? const []))
        Bucket.fromJson(b as Json),
    ];
  }

  /// Creates a bucket.
  Future<Bucket> createBucket(
    String name, {
    String? visibility,
    bool? versioning,
    List<String>? allowedContentTypes,
    int? maxObjectBytes,
  }) async =>
      Bucket.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets',
          method: 'POST',
          body: {
            'name': name,
            if (visibility != null) 'visibility': visibility,
            if (versioning != null) 'versioning': versioning,
            if (allowedContentTypes != null)
              'allowed_content_types': allowedContentTypes,
            if (maxObjectBytes != null) 'max_object_bytes': maxObjectBytes,
          },
        ),
      );

  /// One bucket.
  Future<Bucket> getBucket(String name) async => Bucket.fromJson(
        await _t.requestJson('/api/v1/storage/buckets/${_enc(name)}'),
      );

  /// Changes a bucket's policy.
  Future<Bucket> updateBucket(
    String name, {
    String? visibility,
    bool? versioning,
    List<String>? allowedContentTypes,
    int? maxObjectBytes,
  }) async =>
      Bucket.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(name)}',
          method: 'PATCH',
          body: {
            if (visibility != null) 'visibility': visibility,
            if (versioning != null) 'versioning': versioning,
            if (allowedContentTypes != null)
              'allowed_content_types': allowedContentTypes,
            if (maxObjectBytes != null) 'max_object_bytes': maxObjectBytes,
          },
        ),
      );

  /// Deletes a bucket; [force] removes its objects too.
  Future<void> deleteBucket(String name, {bool force = false}) async {
    await _t.request(
      '/api/v1/storage/buckets/${_enc(name)}',
      method: 'DELETE',
      query: {'force': force ? true : null},
    );
  }

  /// Space used by a bucket (bucket admins).
  Future<Json> bucketUsage(String name) =>
      _t.requestJson('/api/v1/storage/buckets/${_enc(name)}/usage');

  // ---- objects ----

  /// Uploads an object in one request (retried on transient failures: a PUT
  /// is idempotent).
  Future<ObjectInfo> putObject(
    String bucket,
    String key,
    List<int> bytes, {
    String contentType = 'application/octet-stream',
    String? checksumSha256,
    Map<String, String>? metadata,
  }) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          _obj(bucket, key),
          method: 'PUT',
          rawBody: bytes,
          headers: {
            'Content-Type': contentType,
            if (checksumSha256 != null)
              'X-ForgeBase-Checksum-SHA256': checksumSha256,
            for (final e in (metadata ?? const <String, String>{}).entries)
              'X-ForgeBase-Meta-${e.key}': e.value,
          },
        ),
      );

  /// Downloads an object (a specific [versionId] when given).
  Future<Uint8List> getObject(String bucket, String key,
      {String? versionId}) async {
    final res = await _t.send(
      _obj(bucket, key),
      query: {'version': versionId},
      headers: {'Accept': '*/*'},
    );
    return res.stream.toBytes();
  }

  /// Downloads bytes `start..=end` (to the end when [end] is `null`).
  Future<Uint8List> getObjectRange(
    String bucket,
    String key,
    int start, [
    int? end,
  ]) async {
    final res = await _t.send(
      _obj(bucket, key),
      headers: {'Accept': '*/*', 'Range': 'bytes=$start-${end ?? ''}'},
    );
    return res.stream.toBytes();
  }

  /// Starts a streaming download.
  Future<ObjectStream> getObjectStream(
    String bucket,
    String key, {
    String? versionId,
  }) async =>
      ObjectStream(
        await _t.send(
          _obj(bucket, key),
          query: {'version': versionId},
          headers: {'Accept': '*/*'},
        ),
      );

  /// Metadata of an object without reading its bytes.
  Future<ObjectInfo> metadata(String bucket, String key,
          {String? versionId}) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/meta/${keyPath(key)}',
          query: {'version': versionId},
        ),
      );

  /// Lists objects of a bucket.
  Future<ObjectPage> listObjects(
    String bucket, {
    String? prefix,
    String? delimiter,
    int? limit,
    String? cursor,
  }) async =>
      ObjectPage.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/objects',
          query: {
            'prefix': prefix,
            'delimiter': delimiter,
            'limit': limit,
            'cursor': cursor,
          },
        ),
      );

  /// Deletes an object (in a versioned bucket this adds a delete marker), or
  /// one version with [versionId]. Returns `{delete_marker, version_id}`.
  Future<Json> deleteObject(String bucket, String key,
      {String? versionId}) async {
    final r = await _t.request(
      _obj(bucket, key),
      method: 'DELETE',
      query: {'version': versionId},
    );
    return r is Json ? r : <String, dynamic>{};
  }

  /// Copies an object (no bytes move).
  Future<ObjectInfo> copyObject(
    String bucket,
    String key,
    String destKey, {
    String? destBucket,
    String? versionId,
  }) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/copy',
          method: 'POST',
          body: {
            'key': key,
            'dest_key': destKey,
            if (destBucket != null) 'dest_bucket': destBucket,
            if (versionId != null) 'version_id': versionId,
          },
        ),
      );

  /// Moves (renames) an object.
  Future<ObjectInfo> moveObject(
    String bucket,
    String key,
    String destKey, {
    String? destBucket,
  }) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/move',
          method: 'POST',
          body: {
            'key': key,
            'dest_key': destKey,
            if (destBucket != null) 'dest_bucket': destBucket,
          },
        ),
      );

  /// Every version of a key, newest first.
  Future<List<ObjectInfo>> listVersions(String bucket, String key) async {
    final r = await _t.requestJson(
      '/api/v1/storage/buckets/${_enc(bucket)}/versions/${keyPath(key)}',
    );
    return [
      for (final o in (r['items'] as List<dynamic>? ?? const []))
        ObjectInfo.fromJson(o as Json),
    ];
  }

  /// Restores an earlier version as a new current version.
  Future<ObjectInfo> restoreVersion(
    String bucket,
    String key,
    String versionId,
  ) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/restore',
          method: 'POST',
          body: {'key': key, 'version_id': versionId},
        ),
      );

  // ---- signed URLs ----

  /// Creates a time-limited URL anyone can use to GET (download) or PUT
  /// (upload) one object.
  Future<SignedUrl> sign(
    String bucket,
    String key, {
    String method = 'GET',
    Duration? expiresIn,
    String? contentType,
    String? downloadAs,
    String? versionId,
  }) async =>
      SignedUrl.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/sign',
          method: 'POST',
          body: {
            'key': key,
            'method': method.toUpperCase(),
            if (expiresIn != null) 'expires_in': expiresIn.inSeconds,
            if (contentType != null) 'content_type': contentType,
            if (downloadAs != null) 'download_as': downloadAs,
            if (versionId != null) 'version_id': versionId,
          },
        ),
      );

  /// The full URL of a signed URL, to hand to an image widget or a browser.
  String signedUrlFor(SignedUrl signed) => _absolute(signed.url);

  String _absolute(String url) {
    if (url.startsWith('/')) return '${_t.baseUrl}$url';
    if (url.startsWith(_t.baseUrl)) return url;
    throw ArgumentError.value(url, 'url', 'does not belong to this server');
  }

  /// Downloads through a signed GET URL (no credentials are sent).
  Future<Uint8List> downloadSigned(SignedUrl signed) async {
    final res = await _t.send(
      '',
      absoluteUrl: _absolute(signed.url),
      anonymous: true,
      headers: {'Accept': '*/*'},
    );
    return res.stream.toBytes();
  }

  /// Uploads through a signed PUT URL (no credentials are sent).
  Future<ObjectInfo> uploadSigned(
    SignedUrl signed,
    List<int> bytes, {
    String contentType = 'application/octet-stream',
  }) async {
    final res = await _t.send(
      '',
      method: 'PUT',
      absoluteUrl: _absolute(signed.url),
      anonymous: true,
      rawBody: bytes,
      headers: {'Content-Type': contentType},
    );
    final body = await res.stream.bytesToString();
    final j = tryDecodeJson(body);
    if (j is! Json) throw InvalidResponseException('expected an object');
    return ObjectInfo.fromJson(j);
  }

  // ---- multipart and resumable uploads ----

  /// Opens an upload session. [kind] is `multipart` (default) or
  /// `resumable`.
  Future<Upload> createUpload(
    String bucket,
    String key, {
    String? kind,
    String? contentType,
    int? size,
    String? checksumSha256,
    Map<String, String>? metadata,
  }) async =>
      Upload.fromJson(
        await _t.requestJson(
          '/api/v1/storage/buckets/${_enc(bucket)}/uploads',
          method: 'POST',
          body: {
            'key': key,
            if (kind != null) 'kind': kind,
            if (contentType != null) 'content_type': contentType,
            if (size != null) 'size': size,
            if (checksumSha256 != null) 'checksum_sha256': checksumSha256,
            if (metadata != null) 'metadata': metadata,
          },
        ),
      );

  /// State of an upload session (`receivedBytes`, parts): use it to resume.
  Future<Upload> getUpload(String id) async => Upload.fromJson(
        await _t.requestJson('/api/v1/storage/uploads/${_enc(id)}'),
      );

  /// Uploads (or replaces) part [n] (1-based) of a multipart session.
  Future<PartInfo> uploadPart(String id, int n, List<int> bytes) async =>
      PartInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/uploads/${_enc(id)}/parts/$n',
          method: 'PUT',
          rawBody: bytes,
          headers: {'Content-Type': 'application/octet-stream'},
        ),
      );

  /// Appends a chunk at [offset] (resumable upload). Returns the session, or
  /// the finished object when the declared size was reached.
  Future<({Upload? session, ObjectInfo? object})> appendChunk(
    String id,
    int offset,
    List<int> bytes,
  ) async {
    final res = await _t.send(
      '/api/v1/storage/uploads/${_enc(id)}',
      method: 'PATCH',
      rawBody: bytes,
      headers: {
        'Upload-Offset': '$offset',
        'Content-Type': 'application/offset+octet-stream',
      },
    );
    final j = tryDecodeJson(await res.stream.bytesToString());
    if (j is! Json) throw InvalidResponseException('expected an object');
    return res.statusCode == 201
        ? (session: null, object: ObjectInfo.fromJson(j))
        : (session: Upload.fromJson(j), object: null);
  }

  /// Assembles the parts into the object. Without [parts] every received part
  /// is used.
  Future<ObjectInfo> completeUpload(
    String id, {
    List<PartInfo>? parts,
    String? checksumSha256,
  }) async =>
      ObjectInfo.fromJson(
        await _t.requestJson(
          '/api/v1/storage/uploads/${_enc(id)}/complete',
          method: 'POST',
          body: {
            if (parts != null)
              'parts': [
                for (final p in parts)
                  {
                    'part_number': p.partNumber,
                    'checksum_sha256': p.checksumSha256,
                  },
              ],
            if (checksumSha256 != null) 'checksum_sha256': checksumSha256,
          },
        ),
      );

  /// Cancels an upload session.
  Future<void> abortUpload(String id) async {
    await _t.request('/api/v1/storage/uploads/${_enc(id)}', method: 'DELETE');
  }

  /// Uploads [data] as a multipart object in [partSize] pieces (aborts the
  /// session on failure). [onProgress] gets the bytes sent so far.
  Future<ObjectInfo> uploadMultipart(
    String bucket,
    String key,
    List<int> data, {
    int partSize = 8 * 1024 * 1024,
    String? contentType,
    Map<String, String>? metadata,
    void Function(int sent, int total)? onProgress,
  }) async {
    final size = partSize < 1 ? 1 : partSize;
    final up = await createUpload(
      bucket,
      key,
      kind: 'multipart',
      contentType: contentType,
      size: data.length,
      metadata: metadata,
    );
    try {
      final parts = <PartInfo>[];
      var sent = 0;
      for (var off = 0, n = 1; off < data.length; off += size, n++) {
        final end = off + size < data.length ? off + size : data.length;
        parts.add(await uploadPart(up.id, n, data.sublist(off, end)));
        sent = end;
        onProgress?.call(sent, data.length);
      }
      return await completeUpload(up.id, parts: parts);
    } on Object {
      try {
        await abortUpload(up.id);
      } on Object {
        // The original failure matters more.
      }
      rethrow;
    }
  }

  /// Uploads [data] through a resumable session in [chunkSize] pieces. Pass
  /// [resumeId] (a session from an interrupted call) to continue where the
  /// server stopped.
  Future<ObjectInfo> uploadResumable(
    String bucket,
    String key,
    List<int> data, {
    int chunkSize = 8 * 1024 * 1024,
    String? contentType,
    String? resumeId,
    void Function(Upload session)? onSession,
    void Function(int sent, int total)? onProgress,
  }) async {
    final step = chunkSize < 1 ? 1 : chunkSize;
    final up = resumeId != null
        ? await getUpload(resumeId)
        : await createUpload(
            bucket,
            key,
            kind: 'resumable',
            contentType: contentType,
            size: data.length,
          );
    onSession?.call(up);
    var offset = up.receivedBytes;
    if (data.isEmpty) return completeUpload(up.id);
    while (offset < data.length) {
      final end = offset + step < data.length ? offset + step : data.length;
      final r = await appendChunk(up.id, offset, data.sublist(offset, end));
      offset = end;
      onProgress?.call(offset, data.length);
      if (r.object != null) return r.object!;
    }
    return completeUpload(up.id);
  }
}
