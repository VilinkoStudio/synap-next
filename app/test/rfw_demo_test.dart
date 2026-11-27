import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:synap_app/shell/rfw_demo.dart';

void main() {
  testWidgets('rfw reader demo parses and renders', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: RfwReaderDemo()));
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(find.text('RFW Reader 演示'), findsWidgets);
    expect(find.text('#rfw'), findsOneWidget);
  });

  testWidgets('rfw toggle fires write-back event', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: RfwReaderDemo()));
    await tester.pump();
    await tester.tap(find.text('标记完成'));
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(find.text('已完成，点我重开'), findsOneWidget);
  });
}
