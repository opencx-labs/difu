import Foundation

/// The protocol version this client speaks.
public let remoteProtocolVersion = 1

/// The server's answer to `pair` / `auth` / password pairing.
public struct Welcome: Sendable, Equatable {
    public var protocolVersion: Int
    public var device: String?
    /// Only present after pairing.
    public var token: String?
    public var server: ServerInfo
    /// The access level granted to this device; nil from older computers.
    public var access: DeviceAccess? = nil
    /// Password pairing only: the server's proof that it knows the password.
    public var proof: String? = nil
}

/// Push events from the server.
public enum RemoteEvent: Sendable {
    case sessions([SessionSummary])
    case session(SessionView)
    case sessionDelta(SessionDelta)
    case sessionClosed(id: String, message: String?)
    /// Another device was paired with this server.
    case devicePaired(PairedDevice)
    case unknown(String)
}

/// The first frame the client sends.
enum Hello {
    case pair(code: String, deviceId: String, deviceName: String, access: DeviceAccess)
    case auth(token: String)
    case password(deviceId: String, deviceName: String, access: DeviceAccess, nonce: String)

    var json: String {
        switch self {
        case .pair(let code, let deviceId, let deviceName, let access):
            return JSONValue.object([
                "type": "pair", "code": .string(code),
                "device": Self.device(deviceId, deviceName), "access": .string(access.rawValue),
            ]).jsonString()
        case .auth(let token):
            return JSONValue.object(["type": "auth", "token": .string(token)]).jsonString()
        case .password(let deviceId, let deviceName, let access, let nonce):
            return JSONValue.object([
                "type": "password", "device": Self.device(deviceId, deviceName),
                "access": .string(access.rawValue), "nonce": .string(nonce),
            ]).jsonString()
        }
    }

    private static func device(_ id: String, _ name: String) -> JSONValue {
        ["id": .string(id), "name": .string(name)]
    }
}

/// A server frame during the handshake: `welcome`, `denied` or `challenge`.
struct HelloReply: Decodable {
    var type: String
    var message: String?
    var welcome: Welcome?
    var challenge: PasswordChallenge?

    init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        type = f.string("type") ?? ""
        message = f.string("message")
        switch type {
        case "welcome":
            welcome = Welcome(
                protocolVersion: f.int("protocol") ?? remoteProtocolVersion,
                device: f.string("device"),
                token: f.string("token").flatMap { $0.isEmpty ? nil : $0 },
                server: f.value("server", as: ServerInfo.self)
                    ?? ServerInfo(name: "Computer", version: nil, hosts: [], port: nil, fingerprint: nil),
                access: f.string("access").map { DeviceAccess(lenient: $0) },
                proof: f.string("proof").flatMap { $0.isEmpty ? nil : $0 })
        case "challenge":
            if let salt = f.string("salt"), let iterations = f.int("iterations"),
                let nonce = f.string("nonce"), !salt.isEmpty, !nonce.isEmpty
            {
                challenge = PasswordChallenge(salt: salt, iterations: iterations, nonce: nonce)
            }
        default:
            break
        }
    }

    init(_ data: Data) throws {
        do {
            self = try JSONDecoder().decode(HelloReply.self, from: data)
        } catch {
            throw RemoteError.protocolError("the server's message wasn't JSON")
        }
    }
}

/// Any later frame: a response (`id`) or a push event (`event`).
struct IncomingFrame: Decodable {
    var id: Int?
    var error: String?
    var event: RemoteEvent?

    init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        if let id = f.int("id") {
            self.id = id
            if f.has("error") {
                error = f.string("error")
                    ?? f.value("error", as: JSONValue.self)?["message"]?.stringValue
                    ?? "The request failed."
            }
            return
        }
        guard let name = f.string("event") else { return }
        switch name {
        case "sessions":
            var list: [SessionSummary] = f.list("data")
            if list.isEmpty, let wrapped = f.value("data", as: SessionsResult.self) {
                list = wrapped.sessions  // tolerate {"sessions": [...]}
            }
            event = .sessions(list)
        case "session":
            event = f.value("data", as: SessionView.self).map(RemoteEvent.session) ?? .unknown(name)
        case "session.delta":
            event = f.value("data", as: SessionDelta.self).map(RemoteEvent.sessionDelta)
                ?? .unknown(name)
        case "session.closed":
            let data = f.value("data", as: JSONValue.self)
            if let id = data?["id"]?.stringValue {
                event = .sessionClosed(id: id, message: data?["message"]?.stringValue)
            } else {
                event = .unknown(name)
            }
        case "device.paired":
            event = f.value("data", as: PairedDevice.self).map(RemoteEvent.devicePaired)
                ?? .unknown(name)
        default:
            event = .unknown(name)
        }
    }
}

/// `{"result": T}`.
struct ResultEnvelope<T: Decodable>: Decodable {
    var result: T
}

struct SessionsResult: Decodable {
    var sessions: [SessionSummary]

    init(from decoder: Decoder) throws {
        sessions = try decoder.fields().list("sessions")
    }
}
