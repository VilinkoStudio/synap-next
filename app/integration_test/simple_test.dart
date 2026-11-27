import 'package:flutter_test/flutter_test.dart';
import 'package:synap_app/main.dart';
import 'package:synap_app/bridge_generated/frb_generated.dart';
import 'package:synap_app/bridge_generated/api.dart' as api;
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await RustLib.init());
  testWidgets('rust bridge loads and workbench renders', (WidgetTester tester) async {
    // 原生资产能加载 + Rust 函数可调，是 flutter 自动编 Rust 的端到端证明。
    expect(await api.bridgePing(), 'synap-bridge ok');
    await tester.pumpWidget(const SynapApp());
    expect(find.text('synap-next 工作台'), findsOneWidget);
  });
}
