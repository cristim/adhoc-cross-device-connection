// BLECentral.swift — CoreBluetooth central for `ac-dc send-key` (the SENDER).
//
// Validated against real hardware: a macOS 26.1 send to a Linux `receive-key`
// completed with matching SAS and a transferred keys.json. All the
// security-relevant logic lives in Transfer.swift (pinned by tests to vectors
// from the Rust src/transfer.rs); this file only moves bytes.
//
// It mirrors the handshake in Transfer.swift's WIRE FORMAT block: scan for the
// service, read the receiver's pubkey, write our pubkey, then stream the sealed
// keys.json as length-prefixed frames.

import Foundation
import CoreBluetooth
import CryptoKit

// Custom 128-bit UUIDs — identical to src/transfer_ble.rs.
private let kServiceUUID = CBUUID(string: "ACDC5A5A-7C1D-4E2B-9F00-000000000001")
private let kReceiverPubUUID = CBUUID(string: "ACDC5A5A-7C1D-4E2B-9F00-000000000002")
private let kSenderPubUUID = CBUUID(string: "ACDC5A5A-7C1D-4E2B-9F00-000000000003")
private let kPayloadUUID = CBUUID(string: "ACDC5A5A-7C1D-4E2B-9F00-000000000004")

final class Sender: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    private let payload: Data // the plaintext keys.json bytes to send
    private var central: CBCentralManager!
    private var peripheral: CBPeripheral?

    // Our ephemeral X25519 keypair.
    private let ourSecret = Curve25519.KeyAgreement.PrivateKey()
    private var senderPub: Data { ourSecret.publicKey.rawRepresentation }

    private var receiverPubChar: CBCharacteristic?
    private var senderPubChar: CBCharacteristic?
    private var payloadChar: CBCharacteristic?

    private var frames: [Data] = []
    private var frameIndex = 0

    init(payload: Data) {
        self.payload = payload
        super.init()
    }

    func run() {
        central = CBCentralManager(delegate: self, queue: nil)
        // Drive the run loop until the process exits (finish() calls exit()).
        RunLoop.main.run()
    }

    private func finish(_ code: Int32, _ message: String) {
        print(message)
        exit(code)
    }

    // MARK: CBCentralManagerDelegate

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            print("Scanning for a Linux box running `ac-dc receive-key`...")
            central.scanForPeripherals(withServices: [kServiceUUID], options: nil)
        case .unauthorized:
            finish(1, "Bluetooth permission denied. Grant it in System Settings > Privacy.")
        case .poweredOff:
            finish(1, "Bluetooth is powered off.")
        default:
            break
        }
    }

    func centralManager(_ central: CBCentralManager,
                        didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any],
                        rssi RSSI: NSNumber) {
        central.stopScan()
        self.peripheral = peripheral
        peripheral.delegate = self
        print("Found receiver; connecting...")
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        peripheral.discoverServices([kServiceUUID])
    }

    func centralManager(_ central: CBCentralManager,
                        didFailToConnect peripheral: CBPeripheral, error: Error?) {
        finish(1, "Failed to connect: \(error?.localizedDescription ?? "unknown")")
    }

    // MARK: CBPeripheralDelegate

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard let svc = peripheral.services?.first(where: { $0.uuid == kServiceUUID }) else {
            finish(1, "Receiver did not expose the key-transfer service.")
            return
        }
        peripheral.discoverCharacteristics(
            [kReceiverPubUUID, kSenderPubUUID, kPayloadUUID], for: svc)
    }

    func peripheral(_ peripheral: CBPeripheral,
                    didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        for ch in service.characteristics ?? [] {
            switch ch.uuid {
            case kReceiverPubUUID: receiverPubChar = ch
            case kSenderPubUUID: senderPubChar = ch
            case kPayloadUUID: payloadChar = ch
            default: break
            }
        }
        guard receiverPubChar != nil, senderPubChar != nil, payloadChar != nil else {
            finish(1, "Receiver is missing one or more expected characteristics.")
            return
        }
        // Step 2 begins: read the receiver's public key.
        peripheral.readValue(for: receiverPubChar!)
    }

    func peripheral(_ peripheral: CBPeripheral,
                    didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard characteristic.uuid == kReceiverPubUUID else { return }
        guard let receiverPub = characteristic.value, receiverPub.count == 32 else {
            finish(1, "Receiver public key was missing or not 32 bytes.")
            return
        }

        // ECDH + session key + SAS (fixed transcript order receiver||sender).
        do {
            let peerKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: receiverPub)
            let shared = try ourSecret.sharedSecretFromKeyAgreement(with: peerKey)
            let key = Transfer.sessionKey(shared: shared,
                                          receiverPub: receiverPub,
                                          senderPub: senderPub)
            let sas = Transfer.sas(receiverPub: receiverPub, senderPub: senderPub)

            print("")
            print("  Security code (SAS): \(sas)")
            print("")
            print("Confirm this MATCHES the code shown on the Linux box before you")
            print("accept there. If they differ, abort — someone may be in the middle.")
            print("")

            let sealed = try Transfer.seal(key: key, plaintext: payload)
            let mtu = peripheral.maximumWriteValueLength(for: .withoutResponse)
            // Leave a little headroom under the negotiated MTU.
            frames = Transfer.framePayload(sealed: sealed, maxFrame: max(20, mtu))
        } catch {
            finish(1, "Handshake failed: \(error.localizedDescription)")
            return
        }

        // Step 2 (cont.): write our public key, then stream the payload.
        peripheral.writeValue(senderPub, for: senderPubChar!, type: .withResponse)
    }

    func peripheral(_ peripheral: CBPeripheral,
                    didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error = error {
            finish(1, "Write failed: \(error.localizedDescription)")
            return
        }
        if characteristic.uuid == kSenderPubUUID {
            frameIndex = 0
            writeNextFrame()
        } else if characteristic.uuid == kPayloadUUID {
            writeNextFrame()
        }
    }

    private func writeNextFrame() {
        guard let peripheral = peripheral, let ch = payloadChar else { return }
        if frameIndex >= frames.count {
            print("All \(frames.count) frames sent. Waiting on the receiver to confirm the")
            print("code and write keys.json, then you can quit here (Ctrl-C).")
            return
        }
        let frame = frames[frameIndex]
        frameIndex += 1
        // Use .withResponse so didWrite drives the next frame in order.
        peripheral.writeValue(frame, for: ch, type: .withResponse)
    }
}
