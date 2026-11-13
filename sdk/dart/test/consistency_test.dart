import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';

void main() {
  test('keeps the highest position per set', () {
    final t = CommitTokens()
      ..note('control/16/B374D848, s1/0/9')
      ..note('control/16/B374D847')
      ..note('control/17/1');
    expect(t.get('control'), '17/1');
    expect(t.get('s1'), '0/9');
    expect(t.header(), 'control/17/1, s1/0/9');
  });

  test('ignores malformed tokens and empty input', () {
    final t = CommitTokens()
      ..note(null)
      ..note('')
      ..note('nonsense')
      ..note('a/zz/1')
      ..note('a/1/2/3')
      ..note('/1/2');
    expect(t.header(), isNull);
  });

  test('a lower position never wins and clear forgets', () {
    final t = CommitTokens()
      ..note('s/2/0')
      ..note('s/1/FFFFFFFF');
    expect(t.get('s'), '2/0');
    t.clear();
    expect(t.header(), isNull);
  });

  test('caps the number of tracked sets', () {
    final t = CommitTokens();
    for (var i = 0; i < 100; i++) {
      t.note('s$i/0/1');
    }
    expect(t.header()!.split(', ').length, 64);
  });
}
