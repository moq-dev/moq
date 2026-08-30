import 'package:flutter/material.dart';
import 'package:moq/moq.dart';

void main() => runApp(const MoqExample());

class MoqExample extends StatelessWidget {
  const MoqExample({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'MoQ Flutter example',
      theme: ThemeData(colorSchemeSeed: Colors.indigo),
      home: const ConnectPage(),
    );
  }
}

class ConnectPage extends StatefulWidget {
  const ConnectPage({super.key});

  @override
  State<ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<ConnectPage> {
  final url = TextEditingController(text: 'https://relay.example.com');
  Moq? connection;
  String status = 'Disconnected';

  Future<void> connect() async {
    setState(() => status = 'Connecting');
    try {
      final next = await Moq.connect(url.text);
      connection?.close();
      setState(() {
        connection = next;
        status = 'Connected';
      });
    } catch (error) {
      setState(() => status = 'Connection failed: $error');
    }
  }

  @override
  void dispose() {
    connection?.close();
    url.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('MoQ')),
      body: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          children: [
            TextField(
              controller: url,
              decoration: const InputDecoration(labelText: 'Relay URL'),
            ),
            const SizedBox(height: 16),
            FilledButton(onPressed: connect, child: const Text('Connect')),
            const SizedBox(height: 16),
            Text(status),
          ],
        ),
      ),
    );
  }
}
