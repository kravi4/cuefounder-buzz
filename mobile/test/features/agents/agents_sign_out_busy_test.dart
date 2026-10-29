import 'dart:async';

import 'package:buzz/features/agents/create_agent_sheet.dart';
import 'package:buzz/shared/hosted/hosted_environment.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'agents_harness.dart';

void main() {
  testWidgets(
    'signing out while Create agent is busy still closes both sheets',
    (tester) async {
      final h = await AgentsHarness.create();
      await h.open(tester, directory: ['a' * 64]);
      await tester.tap(find.byTooltip('Add agent'));
      await tester.pumpAndSettle();
      expect(find.byType(CreateAgentSheet), findsOneWidget);

      // A request in flight makes Create refuse ordinary pops.
      final pending = Completer<void>();
      unawaited(h.controller.run(() => pending.future));
      await tester.pump();
      expect(h.controller.busy, isTrue);

      h.registry.signedIn = false;
      await tester.pumpAndSettle();
      expect(find.byType(BottomSheet), findsNothing);
      expect(find.text('Open Agents'), findsOneWidget);
      pending.complete();
      await tester.pumpAndSettle();
    },
    skip: !HostedEnvironment.enabled,
  );
}
