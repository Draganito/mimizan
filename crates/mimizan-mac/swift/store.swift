// The one thing the Mac App Store build sells: the TIFF and the negative
// for another program. The JPEG at full size is free. The purchase is the
// non-consumable the iPhone sells, through StoreKit 2 and the same App
// Store record, so one purchase holds on both and for the family.
//
// StoreKit 2 is Swift alone, and the window is Rust: this file is compiled
// to a static library by the crate's build script and called through the
// C functions at its end. The download on GitHub is built without it.
//
// The last known answer is kept in the defaults, so the window does not
// wait on the store at launch; `Transaction.currentEntitlements` is the
// truth and corrects it on every start.
import Foundation
import StoreKit

/// The product in App Store Connect: non-consumable, Family Sharing on.
private let productID = "ch.bojovic.mimizanlab.fullres"
private let ownedKey = "entitlement_fullres"

private final class Store: @unchecked Sendable {
    static let shared = Store()

    private let lock = NSLock()
    private var owned = UserDefaults.standard.bool(forKey: ownedKey)
    private var busy = false
    private var product: Product?
    private var message = ""
    private var started = false
    /// The window's wish to be painted again, and what it is called with.
    private var notify: (@convention(c) (UnsafeMutableRawPointer?) -> Void)?
    private var context: UnsafeMutableRawPointer?

    /// The state is read by the window's thread and written by the tasks.
    @discardableResult
    private func locked<T>(_ body: () -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }

    private func changed() {
        let (notify, context) = locked { (self.notify, self.context) }
        notify?(context)
    }

    private func say(_ text: String) {
        locked { message = text }
        changed()
    }

    /// One purchase or restore at a time.
    private func begin() -> Bool {
        let free: Bool = locked {
            if busy { return false }
            busy = true
            message = ""
            return true
        }
        if free { changed() }
        return free
    }

    private func end() {
        locked { busy = false }
        changed()
    }

    func start(
        _ notify: (@convention(c) (UnsafeMutableRawPointer?) -> Void)?,
        _ context: UnsafeMutableRawPointer?
    ) {
        let first: Bool = locked {
            self.notify = notify
            self.context = context
            if started { return false }
            started = true
            return true
        }
        guard first else { return }
        // Purchases made elsewhere (the iPhone, Family Sharing, a refund)
        // arrive here.
        Task.detached {
            for await result in StoreKit.Transaction.updates {
                switch result {
                case let .verified(transaction):
                    await transaction.finish()
                    await Store.shared.refresh()
                case .unverified:
                    // Never granted and not finished: finishing would drop
                    // a purchase the store may yet verify.
                    NSLog("mimizan store: an unverified transaction was left alone")
                }
            }
        }
        Task.detached {
            await Store.shared.refresh()
            await Store.shared.loadProduct()
        }
    }

    /// Bit 0: owned. Bit 1: a purchase or restore is under way.
    func state() -> UInt32 {
        locked { (owned ? 1 : 0) | (busy ? 2 : 0) }
    }

    /// The price as the store shows it in the user's currency; empty until
    /// the product is loaded.
    func price() -> String {
        locked { product?.displayPrice ?? "" }
    }

    func lastMessage() -> String {
        locked { message }
    }

    func clearMessage() {
        locked { message = "" }
    }

    /// Ask the store what this account owns.
    func refresh() async {
        var found = false
        for await result in StoreKit.Transaction.currentEntitlements {
            if case let .verified(transaction) = result,
                transaction.productID == productID,
                transaction.revocationDate == nil
            {
                found = true
            }
        }
        let owns = found
        let moved: Bool = locked {
            if owned == owns { return false }
            owned = owns
            return true
        }
        if moved {
            UserDefaults.standard.set(owns, forKey: ownedKey)
            NSLog("mimizan store: %@", owns ? "owned" : "not owned")
            changed()
        }
    }

    func loadProduct() async {
        do {
            guard let found = try await Product.products(for: [productID]).first else {
                NSLog("mimizan store: the product is not in the store")
                return
            }
            locked { product = found }
            changed()
        } catch {
            NSLog("mimizan store: products: %@", error.localizedDescription)
        }
    }

    func purchase() async {
        guard begin() else { return }
        defer { end() }
        if locked({ product == nil }) {
            await loadProduct()
        }
        guard let product = locked({ self.product }) else {
            say("The App Store did not answer. Try again in a moment.")
            return
        }
        do {
            switch try await product.purchase() {
            case let .success(verification):
                switch verification {
                case let .verified(transaction):
                    await transaction.finish()
                    await refresh()
                    say("Unlocked. Thank you.")
                case .unverified:
                    say("The purchase could not be verified.")
                }
            case .userCancelled:
                break
            case .pending:
                say("The purchase is waiting for approval.")
            @unknown default:
                break
            }
        } catch {
            say(error.localizedDescription)
        }
    }

    /// Apple's required "Restore Purchases": sync with the account, then
    /// look again.
    func restore() async {
        guard begin() else { return }
        defer { end() }
        do {
            try await AppStore.sync()
            await refresh()
            let owns: Bool = locked { owned }
            say(owns ? "The purchase is restored." : "No purchase found on this account.")
        } catch StoreKitError.userCancelled {
            // The sign-in was put away: nothing to say.
        } catch {
            say(error.localizedDescription)
        }
    }
}

/// As many bytes of `text` as `capacity` holds, written to `buffer`; the
/// count written. UTF-8, no terminator.
private func copy(_ text: String, _ buffer: UnsafeMutablePointer<CChar>?, _ capacity: Int) -> Int {
    let bytes = Array(text.utf8)
    let count = min(bytes.count, capacity)
    guard let buffer = buffer, count > 0 else { return 0 }
    bytes.withUnsafeBufferPointer { source in
        if let base = source.baseAddress {
            UnsafeMutableRawPointer(buffer).copyMemory(from: base, byteCount: count)
        }
    }
    return count
}

// What the Rust side calls (crates/mimizan-mac/src/store.rs).

@_cdecl("mimizan_store_start")
public func mimizanStoreStart(
    _ notify: (@convention(c) (UnsafeMutableRawPointer?) -> Void)?,
    _ context: UnsafeMutableRawPointer?
) {
    Store.shared.start(notify, context)
}

@_cdecl("mimizan_store_state")
public func mimizanStoreState() -> UInt32 {
    Store.shared.state()
}

@_cdecl("mimizan_store_price")
public func mimizanStorePrice(_ buffer: UnsafeMutablePointer<CChar>?, _ capacity: Int) -> Int {
    copy(Store.shared.price(), buffer, capacity)
}

@_cdecl("mimizan_store_message")
public func mimizanStoreMessage(_ buffer: UnsafeMutablePointer<CChar>?, _ capacity: Int) -> Int {
    copy(Store.shared.lastMessage(), buffer, capacity)
}

@_cdecl("mimizan_store_clear_message")
public func mimizanStoreClearMessage() {
    Store.shared.clearMessage()
}

@_cdecl("mimizan_store_purchase")
public func mimizanStorePurchase() {
    Task.detached { await Store.shared.purchase() }
}

@_cdecl("mimizan_store_restore")
public func mimizanStoreRestore() {
    Task.detached { await Store.shared.restore() }
}
