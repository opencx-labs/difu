import Foundation
import AppKit
import UserNotifications

// One structured request per invocation. Only "authorize" can ask permission.
struct Request: Decodable { let action: String; let title: String?; let event: String? }
struct Reply: Encodable { var desktop: String; var sound: String; var error: String? }
let center = UNUserNotificationCenter.current()
var finished = false
func respond(_ reply: Reply) {
    DispatchQueue.main.async {
        if let data = try? JSONEncoder().encode(reply) { FileHandle.standardOutput.write(data) }
        finished = true
    }
}
func status(_ completion: @escaping (Reply) -> Void) {
    center.getNotificationSettings { settings in
        let desktop: String
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: desktop = "Allowed"
        case .denied: desktop = "Denied — enable Difu in System Settings > Notifications"
        case .notDetermined: desktop = "Not requested"
        @unknown default: desktop = "Unavailable"
        }
        DispatchQueue.main.async {
            completion(Reply(desktop: desktop, sound: NSSound(named: "Glass") == nil ? "Unavailable" : "Available", error: nil))
        }
    }
}
do {
    let request = try JSONDecoder().decode(Request.self, from: FileHandle.standardInput.readDataToEndOfFile())
    switch request.action {
    case "status": status(respond)
    case "authorize":
        center.requestAuthorization(options: [.alert]) { _, error in
            status { reply in
                var reply = reply
                if let error = error { reply.error = error.localizedDescription }
                respond(reply)
            }
        }
    case "deliver":
        center.getNotificationSettings { settings in
            guard settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional else {
                status { reply in
                    var reply = reply
                    reply.error = "Desktop notifications are not authorized; use Enable or Test in Settings"
                    respond(reply)
                }
                return
            }
            let content = UNMutableNotificationContent()
            content.title = String((request.title ?? "Difu").prefix(100))
            content.body = request.event ?? "Session activity"
            // Deliberately no UNNotificationSound: standalone preview/real audio share NSSound.
            let item = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
            center.add(item) { error in
                status { reply in
                    var reply = reply
                    if let error = error { reply.error = error.localizedDescription }
                    respond(reply)
                }
            }
        }
    case "sound":
        guard let sound = NSSound(named: "Glass"), sound.play() else {
            respond(Reply(desktop: "Not checked", sound: "Unavailable", error: "Cannot start notification sound"))
            break
        }
        let deadline = Date().addingTimeInterval(3)
        while sound.isPlaying && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        let timedOut = sound.isPlaying
        if timedOut { sound.stop() }
        respond(Reply(desktop: "Not checked", sound: timedOut ? "Error" : "Available", error: timedOut ? "Sound playback timed out" : nil))
    default: respond(Reply(desktop: "Unavailable", sound: "Unavailable", error: "Unknown notification action"))
    }
} catch { respond(Reply(desktop: "Unavailable", sound: "Unavailable", error: "Invalid notification request")) }
let deadline = Date().addingTimeInterval(29)
while !finished && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
if !finished {
    let reply = Reply(desktop: "Unavailable", sound: "Unavailable", error: "Notification request timed out")
    if let data = try? JSONEncoder().encode(reply) { FileHandle.standardOutput.write(data) }
}
