import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

void main() {
  test('MemoryTokenStorage stores, reads and deletes', () {
    final s = MemoryTokenStorage();
    expect(s.read(), isNull);
    s.write('v');
    expect(s.read(), 'v');
    s.delete();
    expect(s.read(), isNull);
  });

  test('CallbackTokenStorage forwards to the callbacks', () async {
    final calls = <String>[];
    final s = CallbackTokenStorage(
      read: () async {
        calls.add('read');
        return 'x';
      },
      write: (v) => calls.add('write:$v'),
      delete: () => calls.add('delete'),
    );
    expect(await s.read(), 'x');
    await s.write('y');
    await s.delete();
    expect(calls, ['read', 'write:y', 'delete']);
  });

  test('Session survives a JSON round trip', () {
    const s = Session(
      accessToken: 'a',
      refreshToken: 'r',
      expiresAt: 123,
      sessionId: 's',
      sessionExpiresAt: 'e',
      subject: {'kind': 'user'},
      user: {'id': 'u'},
    );
    final back = Session.fromJson(s.toJson());
    expect(back.accessToken, 'a');
    expect(back.refreshToken, 'r');
    expect(back.expiresAt, 123);
    expect(back.subject, {'kind': 'user'});
    expect(back.user, {'id': 'u'});
  });
}
