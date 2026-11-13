import 'dart:convert';

import 'consistency.dart';
import 'errors.dart';
import 'http.dart';

String _enc(String s) => Uri.encodeComponent(s);

/// A record as the API returns it: system fields plus the collection's own.
typedef Row = Map<String, dynamic>;

/// Options of a list call.
class ListOptions {
  /// Creates options.
  const ListOptions({
    this.filter,
    this.params,
    this.sort,
    this.expand,
    this.fields,
    this.search,
    this.page,
    this.perPage,
    this.cursor,
    this.skipTotal,
    this.consistency,
  });

  /// Filter-language expression, e.g. `price > {:min} && status = "active"`.
  final String? filter;

  /// Typed values for `{:name}` placeholders in [filter] (sent with the JSON
  /// query endpoint).
  final Map<String, Object?>? params;

  /// `-created,name`.
  final String? sort;

  /// Relations to expand inline.
  final List<String>? expand;

  /// Fields to return.
  final List<String>? fields;

  /// Full-text search term.
  final String? search;

  /// 1-based page (offset pagination).
  final int? page;

  /// Page size (server maximum 500).
  final int? perPage;

  /// Cursor from a previous page's `nextCursor`.
  final String? cursor;

  /// Skip the total count (cheaper on large collections).
  final bool? skipTotal;

  /// `strong` reads from the primary even when replicas serve reads.
  final Consistency? consistency;

  /// A copy with some fields replaced.
  ListOptions copyWith({String? cursor, int? page, bool clearPage = false}) =>
      ListOptions(
        filter: filter,
        params: params,
        sort: sort,
        expand: expand,
        fields: fields,
        search: search,
        page: clearPage ? null : (page ?? this.page),
        perPage: perPage,
        cursor: cursor ?? this.cursor,
        skipTotal: skipTotal,
        consistency: consistency,
      );
}

/// One page of records.
class ListResult<T> {
  /// Creates a page.
  const ListResult({
    required this.items,
    required this.perPage,
    this.page,
    this.totalItems,
    this.totalPages,
    this.nextCursor,
  });

  /// Reads the server's page JSON.
  factory ListResult.fromJson(Json w, T Function(Json) decode) => ListResult(
        items: [
          for (final i in (w['items'] as List<dynamic>? ?? const []))
            decode(i as Json),
        ],
        page: (w['page'] as num?)?.toInt(),
        perPage: (w['per_page'] as num?)?.toInt() ?? 0,
        totalItems: (w['total_items'] as num?)?.toInt(),
        totalPages: (w['total_pages'] as num?)?.toInt(),
        nextCursor: w['next_cursor'] as String?,
      );

  /// The records.
  final List<T> items;

  /// Page number (offset pagination).
  final int? page;

  /// Page size.
  final int perPage;

  /// Total records, unless `skipTotal`.
  final int? totalItems;

  /// Total pages, unless `skipTotal`.
  final int? totalPages;

  /// Pass as `cursor` for the next page; `null` on the last page.
  final String? nextCursor;
}

/// Formats of export and import.
enum TransferFormat {
  /// A JSON array.
  json,

  /// One JSON object per line.
  ndjson,

  /// Comma-separated values.
  csv,
}

/// Records of one collection: `client.collection('posts')`.
class RecordsApi<T> {
  /// Creates the API. Pass [decode] to map rows to your own type.
  RecordsApi(this._t, this.name, {T Function(Row row)? decode})
      : _decode = decode ?? ((r) => r as T),
        _base = '/api/v1/collections/${_enc(name)}';

  final Transport _t;

  /// Collection name.
  final String name;
  final T Function(Row) _decode;
  final String _base;

  /// One page. With `params` the typed JSON query endpoint is used, otherwise
  /// a plain GET.
  Future<ListResult<T>> list([ListOptions o = const ListOptions()]) async {
    final common = <String, Object?>{
      'filter': o.filter,
      'sort': o.sort,
      'search': o.search,
      'page': o.page,
      'per_page': o.perPage,
      'cursor': o.cursor,
      'skip_total': o.skipTotal,
    };
    final Json w;
    if (o.params != null) {
      w = await _t.requestJson(
        '$_base/records/query',
        method: 'POST',
        consistency: o.consistency,
        body: {
          ...common..removeWhere((_, v) => v == null),
          'params': o.params,
          if (o.expand != null) 'expand': o.expand,
          if (o.fields != null) 'fields': o.fields,
        },
      );
    } else {
      w = await _t.requestJson(
        '$_base/records',
        consistency: o.consistency,
        query: {
          ...common,
          'expand': o.expand?.join(','),
          'fields': o.fields?.join(','),
        },
      );
    }
    return ListResult.fromJson(w, _decode);
  }

  /// Every page, lazily. Follows `nextCursor`, else page numbers.
  Stream<ListResult<T>> pages([ListOptions o = const ListOptions()]) async* {
    var cursor = o.cursor;
    var page = o.page ?? 1;
    while (true) {
      final res = await list(
        cursor != null
            ? o.copyWith(cursor: cursor, clearPage: true)
            : o.copyWith(page: page),
      );
      yield res;
      if (res.nextCursor != null) {
        cursor = res.nextCursor;
      } else if (cursor == null &&
          res.page != null &&
          res.totalPages != null &&
          res.page! < res.totalPages!) {
        page = res.page! + 1;
      } else {
        return;
      }
    }
  }

  /// Every record, lazily: `await for (final p in posts.iterate(...))`.
  Stream<T> iterate([ListOptions o = const ListOptions()]) async* {
    await for (final p in pages(o)) {
      yield* Stream.fromIterable(p.items);
    }
  }

  /// Collects every record (mind the memory use on big collections).
  Future<List<T>> all([ListOptions o = const ListOptions()]) =>
      iterate(o).toList();

  /// First record matching [filter], or `null`.
  Future<T?> first(String filter, {String? sort, List<String>? expand}) async {
    final r = await list(
      ListOptions(
        filter: filter,
        sort: sort,
        expand: expand,
        perPage: 1,
        skipTotal: true,
      ),
    );
    return r.items.isEmpty ? null : r.items.first;
  }

  /// One record by id.
  Future<T> get(
    String id, {
    List<String>? expand,
    List<String>? fields,
    Consistency? consistency,
  }) async =>
      _decode(
        await _t.requestJson(
          '$_base/records/${_enc(id)}',
          consistency: consistency,
          query: {'expand': expand?.join(','), 'fields': fields?.join(',')},
        ),
      );

  /// Creates a record.
  Future<T> create(Row data) async => _decode(
        await _t.requestJson('$_base/records', method: 'POST', body: data),
      );

  /// Partial update (PATCH). With [version] (the record's `_version`) a stale
  /// write throws [ConflictException].
  Future<T> update(String id, Row patch, {int? version}) async => _decode(
        await _t.requestJson(
          '$_base/records/${_enc(id)}',
          method: 'PATCH',
          body: patch,
          headers: _ifMatch(version),
        ),
      );

  /// Full replace (PUT). Same optimistic [version] as [update].
  Future<T> replace(String id, Row data, {int? version}) async => _decode(
        await _t.requestJson(
          '$_base/records/${_enc(id)}',
          method: 'PUT',
          body: data,
          headers: _ifMatch(version),
        ),
      );

  /// Deletes a record.
  Future<void> delete(String id) async {
    await _t.request('$_base/records/${_enc(id)}', method: 'DELETE');
  }

  /// Number of records, optionally filtered.
  Future<int> count([String? filter]) async {
    final r = await _t.requestJson(
      '$_base/records/count',
      query: {'filter': filter},
    );
    return (r['count'] as num).toInt();
  }

  /// Downloads the collection as text.
  Future<String> export([
    TransferFormat format = TransferFormat.json,
    String? filter,
  ]) async =>
      (await _t.request(
        '$_base/export',
        query: {'format': format.name, 'filter': filter},
        text: true,
      ))! as String;

  /// Uploads records as text in the given format.
  Future<Json> import(TransferFormat format, String body) => _t.requestJson(
        '$_base/import',
        method: 'POST',
        query: {'format': format.name},
        rawBody: utf8.encode(body),
        headers: {'Content-Type': 'application/octet-stream'},
      );

  Map<String, String>? _ifMatch(int? version) =>
      version == null ? null : {'If-Match': '"$version"'};
}

/// Collection definitions (`client.collections`).
class CollectionsApi {
  /// Creates the API.
  CollectionsApi(this._t);

  final Transport _t;

  /// Every collection definition.
  Future<List<Json>> list() async {
    final r = await _t.requestJson('/api/v1/collections');
    return [for (final i in (r['items'] as List<dynamic>)) i as Json];
  }

  /// One definition.
  Future<Json> get(String name) =>
      _t.requestJson('/api/v1/collections/${_enc(name)}');

  /// Creates a collection.
  Future<Json> create(Json def) =>
      _t.requestJson('/api/v1/collections', method: 'POST', body: def);

  /// Replaces a definition.
  Future<Json> update(String name, Json def) => _t.requestJson(
        '/api/v1/collections/${_enc(name)}',
        method: 'PUT',
        body: def,
      );

  /// Deletes a collection.
  Future<void> delete(String name) async {
    await _t.request('/api/v1/collections/${_enc(name)}', method: 'DELETE');
  }
}
