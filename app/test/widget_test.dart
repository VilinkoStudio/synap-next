import 'package:flutter_test/flutter_test.dart';

import 'package:synap_app/main.dart';

void main() {
  testWidgets('workbench renders capture + empty reader', (
    WidgetTester tester,
  ) async {
    await tester.pumpWidget(const SynapApp());

    expect(find.text('synap-next 工作台'), findsOneWidget);
    expect(find.text('记一条'), findsOneWidget);
  });
}
