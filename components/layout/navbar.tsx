'use client'

import Link from 'next/link'
import { useSession, signOut } from 'next-auth/react'
import { Button } from '@/components/ui/button'
import { 
  ShoppingBag, 
  MessageSquare, 
  Newspaper, 
  User, 
  LogOut,
  Menu,
  X
} from 'lucide-react'
import { useState } from 'react'

export function Navbar() {
  const { data: session } = useSession()
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)

  return (
    <nav className="border-b border-gray-700 bg-gray-900/95 backdrop-blur supports-[backdrop-filter]:bg-gray-900/75">
      <div className="container mx-auto px-4">
        <div className="flex h-16 items-center justify-between">
          {/* Logo */}
          <Link href="/" className="flex items-center space-x-2">
            <span className="text-2xl font-bold bg-gradient-to-r from-rust-400 to-rust-600 bg-clip-text text-transparent">
              Art of Rust
            </span>
          </Link>

          {/* Desktop Navigation */}
          <div className="hidden md:flex items-center space-x-6">
            <Link href="/news" className="text-gray-300 hover:text-white transition-colors">
              <Newspaper className="w-5 h-5" />
            </Link>
            <Link href="/forum" className="text-gray-300 hover:text-white transition-colors">
              <MessageSquare className="w-5 h-5" />
            </Link>
            <Link href="/shop" className="text-gray-300 hover:text-white transition-colors">
              <ShoppingBag className="w-5 h-5" />
            </Link>
            
            {session ? (
              <>
                <Link href="/dashboard">
                  <Button variant="ghost" size="sm">
                    <User className="w-4 h-4 mr-2" />
                    Dashboard
                  </Button>
                </Link>
                <Button 
                  variant="ghost" 
                  size="sm"
                  onClick={() => signOut()}
                >
                  <LogOut className="w-4 h-4 mr-2" />
                  Sign Out
                </Button>
              </>
            ) : (
              <>
                <Link href="/login">
                  <Button variant="ghost" size="sm">Login</Button>
                </Link>
                <Link href="/register">
                  <Button size="sm">Sign Up</Button>
                </Link>
              </>
            )}
          </div>

          {/* Mobile Menu Button */}
          <button
            className="md:hidden text-gray-300"
            onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
          >
            {mobileMenuOpen ? <X className="w-6 h-6" /> : <Menu className="w-6 h-6" />}
          </button>
        </div>

        {/* Mobile Navigation */}
        {mobileMenuOpen && (
          <div className="md:hidden py-4 space-y-4 border-t border-gray-700">
            <Link 
              href="/news" 
              className="block text-gray-300 hover:text-white"
              onClick={() => setMobileMenuOpen(false)}
            >
              News
            </Link>
            <Link 
              href="/forum" 
              className="block text-gray-300 hover:text-white"
              onClick={() => setMobileMenuOpen(false)}
            >
              Forum
            </Link>
            <Link 
              href="/shop" 
              className="block text-gray-300 hover:text-white"
              onClick={() => setMobileMenuOpen(false)}
            >
              Shop
            </Link>
            {session ? (
              <>
                <Link 
                  href="/dashboard"
                  className="block text-gray-300 hover:text-white"
                  onClick={() => setMobileMenuOpen(false)}
                >
                  Dashboard
                </Link>
                <button
                  onClick={() => {
                    signOut()
                    setMobileMenuOpen(false)
                  }}
                  className="block text-gray-300 hover:text-white"
                >
                  Sign Out
                </button>
              </>
            ) : (
              <>
                <Link 
                  href="/login"
                  className="block text-gray-300 hover:text-white"
                  onClick={() => setMobileMenuOpen(false)}
                >
                  Login
                </Link>
                <Link 
                  href="/register"
                  className="block text-gray-300 hover:text-white"
                  onClick={() => setMobileMenuOpen(false)}
                >
                  Sign Up
                </Link>
              </>
            )}
          </div>
        )}
      </div>
    </nav>
  )
}

