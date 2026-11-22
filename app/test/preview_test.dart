import 'package:flutter_test/flutter_test.dart';
import 'package:synap_app/main.dart';
import 'package:synap_app/shell/debug_preview.dart';

void main() {
  tearDown(() => previewTarget.value = PreviewTarget.system);

  testWidgets('workbench renders on default platform', (tester) async {
    await tester.pumpWidget(const SynapApp());
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(find.text('synap-next 工作台'), findsOneWidget);
  });

  testWidgets('windows preview provides FluentTheme for fluent buttons', (tester) async {
    previewTarget.value = PreviewTarget.windows;
    await tester.pumpWidget(const SynapApp());
    await tester.pump();
    expect(tester.takeException(), isNull);
  });

  testWidgets('web preview renders flat theme', (tester) async {
    previewTarget.value = PreviewTarget.web;
    await tester.pumpWidget(const SynapApp());
    await tester.pump();
    expect(tester.takeException(), isNull);
  });
}
